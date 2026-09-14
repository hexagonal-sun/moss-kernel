use crate::clock::realtime::date;
use crate::process::creds::Credentials;
use crate::{
    drivers::{DM, Driver},
    process::{
        Task,
        inotify::{notify_create, notify_delete, notify_delete_self, notify_modify, notify_move},
    },
    sync::SpinLock,
};
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use async_trait::async_trait;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use dir::DirFile;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{
        BlockDevice, FS_ID_START, FileType, Filesystem, Inode, InodeId, OpenFlags,
        attr::{AccessMode, FilePermissions},
        path::Path,
    },
};
use open_file::OpenFile;
use reg::RegFile;

pub mod dir;
pub mod fops;
pub mod location;
pub mod memfd;
pub mod mount;
use location::Dentry;
pub use location::VfsPath;
use mount::MountNamespace;
pub mod open_file;
mod path_file;
pub mod pipe;
pub mod reg;
pub mod syscalls;

const MAX_SYMLINK: u32 = 40;

/// A dummy inode used as a placeholder before the root filesystem is mounted.
pub struct DummyInode {}

impl Inode for DummyInode {
    fn id(&self) -> InodeId {
        InodeId::dummy()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// This trait represents a type of filesystem, like "ext4" or "tmpfs". It acts
/// as a factory for creating mounted instances.
#[async_trait]
pub trait FilesystemDriver: Driver + Send + Sync {
    async fn construct(
        &self,
        fs_id: u64,
        blk_dev: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>>;
}

struct VfsState {
    filesystems: BTreeMap<u64, Weak<dyn Filesystem>>,
    internal: BTreeMap<u64, Arc<dyn Filesystem>>,
    roots: BTreeMap<u64, Weak<Dentry>>,
}
impl VfsState {
    const fn new() -> Self {
        Self {
            filesystems: BTreeMap::new(),
            internal: BTreeMap::new(),
            roots: BTreeMap::new(),
        }
    }
    fn get_fs(&self, inode_id: InodeId) -> Option<Arc<dyn Filesystem>> {
        self.filesystems
            .get(&inode_id.fs_id())
            .and_then(Weak::upgrade)
    }
}

#[allow(clippy::upper_case_acronyms)]
pub struct VFS {
    next_fs_id: AtomicU64,
    state: SpinLock<VfsState>,
    path_ops: crate::sync::Mutex<()>,
}

impl VFS {
    const fn new() -> Self {
        Self {
            next_fs_id: AtomicU64::new(FS_ID_START),
            state: SpinLock::new(VfsState::new()),
            path_ops: crate::sync::Mutex::new(()),
        }
    }

    /// Registers a kernel-only filesystem without exposing a userspace mount.
    pub(crate) fn register_internal_fs(&self, fs: Arc<dyn Filesystem>) {
        assert!(fs.id() < FS_ID_START);
        let mut state = self.state.lock_save_irq();
        state.filesystems.insert(fs.id(), Arc::downgrade(&fs));
        state.internal.insert(fs.id(), fs);
    }

    /// Creates an instance of a filesystem from a registered driver.
    ///
    /// This does not mount the filesystem, but prepares an instance that can
    /// then be attached to a mount point.
    async fn create_fs_instance(
        &self,
        driver_name: &str,
        blkdev: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        let driver = DM
            .lock_save_irq()
            .find_by_name(driver_name)
            .ok_or(FsError::DriverNotFound)?
            .as_filesystem_driver()
            .ok_or(FsError::DriverNotFound)?;

        let id = self.next_fs_id.fetch_add(1, Ordering::SeqCst);

        let fs = driver.construct(id, blkdev).await?;
        let mut state = self.state.lock_save_irq();
        state.filesystems.retain(|_, fs| fs.strong_count() != 0);
        state.filesystems.insert(fs.id(), Arc::downgrade(&fs));
        Ok(fs)
    }

    async fn fs_root(&self, fs: &Arc<dyn Filesystem>) -> Result<Arc<Dentry>> {
        let inode = fs.root_inode().await?;
        let mut state = self.state.lock_save_irq();
        if let Some(root) = state.roots.get(&fs.id()).and_then(Weak::upgrade) {
            return Ok(root);
        }
        state.roots.retain(|_, d| d.strong_count() != 0);
        let root = Dentry::root(inode);
        state.roots.insert(fs.id(), Arc::downgrade(&root));
        Ok(root)
    }

    pub async fn mount_root(
        &self,
        driver_name: &str,
        blkdev: Option<Box<dyn BlockDevice>>,
    ) -> Result<()> {
        let fs = self.create_fs_instance(driver_name, blkdev).await?;
        let root = self.fs_root(&fs).await?;
        MountNamespace::initial().install_root(fs, root, driver_name);
        Ok(())
    }

    pub async fn mount(
        &self,
        ns: &Arc<MountNamespace>,
        target: VfsPath,
        driver_name: &str,
        blkdev: Option<Box<dyn BlockDevice>>,
        creds: Option<&Credentials>,
    ) -> Result<()> {
        self.mount_flags(ns, target, driver_name, blkdev, creds, 0)
            .await
    }
    pub async fn mount_flags(
        &self,
        ns: &Arc<MountNamespace>,
        target: VfsPath,
        driver_name: &str,
        blkdev: Option<Box<dyn BlockDevice>>,
        creds: Option<&Credentials>,
        flags: u64,
    ) -> Result<()> {
        if target.getattr().await?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory.into());
        }
        let fs = self.create_fs_instance(driver_name, blkdev).await?;
        let root = self.fs_root(&fs).await?;
        if driver_name == "tmpfs"
            && let Some(creds) = creds
        {
            let mut attr = root.inode.getattr().await?;
            attr.uid = creds.fsuid();
            attr.gid = creds.fsgid();
            root.inode.setattr(attr).await?;
        }
        let _guard = self.path_ops.lock().await;
        let target = MountNamespace::follow(target);
        ns.attach_flags(&target, fs, root, driver_name, flags)
    }

    pub async fn bind(
        &self,
        ns: &Arc<MountNamespace>,
        source: VfsPath,
        target: VfsPath,
        recursive: bool,
    ) -> Result<()> {
        if !ns.contains(&source) || !ns.contains(&target) {
            return Err(KernelError::InvalidValue);
        }
        if (source.getattr().await?.file_type == FileType::Directory)
            != (target.getattr().await?.file_type == FileType::Directory)
        {
            return Err(FsError::NotADirectory.into());
        }
        let _guard = self.path_ops.lock().await;
        let target = MountNamespace::follow(target);
        ns.bind(&source, &target, recursive)
    }

    pub async fn get_fs(&self, inode: Arc<dyn Inode>) -> Result<Arc<dyn Filesystem>> {
        self.state
            .lock_save_irq()
            .get_fs(inode.id())
            .ok_or(KernelError::from(FsError::NoDevice))
    }

    /// Resolves a path string to an Inode, starting from a given root for
    /// relative paths.
    pub async fn resolve_path(
        &self,
        path: &Path,
        root: VfsPath,
        task: &Arc<Task>,
    ) -> Result<VfsPath> {
        let creds = task.creds.lock_save_irq().clone();
        self.resolve_with_credentials(path, root, task, true, &creds)
            .await
    }

    /// Resolves a path string to an Inode, starting from a given root for
    /// relative paths, without following the final symbolic link.
    pub async fn resolve_path_nofollow(
        &self,
        path: &Path,
        root: VfsPath,
        task: &Arc<Task>,
    ) -> Result<VfsPath> {
        let creds = task.creds.lock_save_irq().clone();
        self.resolve_with_credentials(path, root, task, false, &creds)
            .await
    }

    pub(crate) async fn resolve_with_credentials(
        &self,
        path: &Path,
        root: VfsPath,
        task: &Arc<Task>,
        follow: bool,
        creds: &Credentials,
    ) -> Result<VfsPath> {
        if path.as_str().is_empty() {
            return Err(FsError::NotFound.into());
        }
        let process_root = task.fs().root.lock_save_irq().clone();
        let root = if path.is_absolute() {
            process_root.clone()
        } else {
            root
        };
        self.resolve_path_internal(path, root, follow, process_root, Some(creds))
            .await
    }

    /// Resolves a path string to an Inode, starting from a given root for
    /// relative paths, and using the filesystem root inode for absolute paths.
    pub async fn resolve_path_absolute(&self, path: &Path, root: VfsPath) -> Result<VfsPath> {
        let root = if path.is_absolute() {
            self.root_path()
        } else {
            root
        };
        self.resolve_path_internal(path, root, true, self.root_path(), None)
            .await
    }

    async fn resolve_path_internal(
        &self,
        path: &Path,
        root: VfsPath,
        follow_last_sym: bool,
        process_root: VfsPath,
        creds: Option<&Credentials>,
    ) -> Result<VfsPath> {
        let _guard = self.path_ops.lock().await;
        let mut current = root;
        let mut symlink_count = 0;
        let mut components: Vec<_> = path
            .as_str()
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned())
            .collect();
        components.reverse();
        while let Some(component) = components.pop() {
            if current.getattr().await?.file_type != FileType::Directory {
                return Err(FsError::NotADirectory.into());
            }
            if let Some(creds) = creds {
                creds
                    .check_inode_access(current.as_ref(), AccessMode::X_OK)
                    .await?;
            }
            if component == "." {
                continue;
            }
            if component == ".." {
                current = MountNamespace::follow(current.parent(&process_root));
                continue;
            }
            // Traverse mounts when looking up a named child, not when starting
            // from a pinned cwd/dirfd or jumping through a proc magic link.
            let next = MountNamespace::follow(current.lookup(&component).await?);
            if next.getattr().await?.file_type == FileType::Symlink
                && (follow_last_sym || !components.is_empty() || path.as_str().ends_with('/'))
            {
                symlink_count += 1;
                if symlink_count > MAX_SYMLINK {
                    return Err(FsError::Loop.into());
                }
                if let Some(target) = crate::drivers::fs::proc::follow_path(next.as_ref())? {
                    current = target;
                    continue;
                }
                if let Some(inode) = next.follow_link().await? {
                    current = VfsPath::anonymous(inode);
                    continue;
                }
                let target = next.readlink().await?;
                let mut more: Vec<_> = target
                    .as_str()
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_owned())
                    .collect();
                if target.as_str().ends_with('/') {
                    more.push(".".to_owned());
                }
                more.reverse();
                components.extend(more);
                if target.is_absolute() {
                    current = process_root.clone();
                }
                continue;
            }
            current = next;
        }
        if path.as_str().ends_with('/') && current.getattr().await?.file_type != FileType::Directory
        {
            return Err(FsError::NotADirectory.into());
        }
        Ok(current)
    }

    pub fn root_path(&self) -> VfsPath {
        MountNamespace::initial().root()
    }

    pub async fn open(
        &self,
        path: &Path,
        flags: OpenFlags,
        root: VfsPath,
        mode: FilePermissions,
        task: &Arc<Task>,
    ) -> Result<Arc<OpenFile>> {
        if path.as_str().is_empty() {
            return Err(FsError::NotFound.into());
        }
        let flags = if flags.contains(OpenFlags::O_PATH) {
            flags & (OpenFlags::O_PATH | OpenFlags::O_DIRECTORY | OpenFlags::O_NOFOLLOW)
        } else {
            flags & !OpenFlags::O_CLOEXEC
        };
        let creds = task.creds.lock_save_irq().clone();
        let mut created = false;
        // Attempt to resolve the full path first.
        let resolve_result = if flags.contains(OpenFlags::O_NOFOLLOW)
            || flags.contains(OpenFlags::O_CREAT | OpenFlags::O_EXCL)
        {
            self.resolve_path_nofollow(path, root.clone(), task).await
        } else {
            self.resolve_path(path, root.clone(), task).await
        };

        let target_inode = match resolve_result {
            // The file/directory exists.
            Ok(inode) => {
                if flags.contains(OpenFlags::O_CREAT | OpenFlags::O_EXCL) {
                    // O_CREAT and O_EXCL were passed and the file exists. This is
                    // an error.
                    return Err(FsError::AlreadyExists.into());
                }
                // The file exists, and we're not exclusively creating. Proceed.
                inode
            }

            // The path was not found.
            Err(KernelError::Fs(FsError::NotFound)) => {
                // If O_CREAT is specified, we should create it.
                if flags.contains(OpenFlags::O_CREAT) {
                    // Determine the target name and parent directory. If the path has no
                    // explicit parent component (e.g., "foo"), use the provided `root`
                    // (cwd or dirfd) as the parent directory.
                    let file_name = path.file_name().ok_or(FsError::InvalidInput)?;
                    let parent_inode = if let Some(parent_path) = path.parent() {
                        self.resolve_path(parent_path, root.clone(), task).await?
                    } else {
                        root.clone()
                    };

                    // Ensure the parent is actually a directory before creating a
                    // file in it.
                    let parent_attr = parent_inode.getattr().await?;
                    if parent_attr.file_type != FileType::Directory {
                        return Err(FsError::NotADirectory.into());
                    }
                    creds.check_file_access(&parent_attr, AccessMode::W_OK | AccessMode::X_OK)?;
                    let mode = FilePermissions::from_bits_truncate(
                        mode.bits() & !(*task.fs().umask.lock_save_irq() as u16),
                    );

                    let _create_lease = parent_inode.begin_write()?;
                    let target_inode = parent_inode
                        .create(file_name, FileType::File, mode, Some(date()))
                        .await?;
                    let target_inode = parent_inode.child(file_name, target_inode);
                    let mut attr = target_inode.getattr().await?;
                    attr.uid = creds.fsuid();
                    attr.gid = if parent_attr.permissions.contains(FilePermissions::S_ISGID) {
                        parent_attr.gid
                    } else {
                        creds.fsgid()
                    };
                    target_inode.setattr(attr).await?;
                    created = true;
                    notify_create(parent_inode.id(), file_name, false).await;
                    target_inode
                } else {
                    // O_CREAT was not specified, so NotFound is the correct error.
                    return Err(FsError::NotFound.into());
                }
            }

            // Some other error occurred during resolution (e.g., NotADirectory
            // mid-path).
            Err(e) => return Err(e),
        };

        let attr = target_inode.getattr().await?;

        if flags.contains(OpenFlags::O_DIRECTORY) && attr.file_type != FileType::Directory {
            return Err(FsError::NotADirectory.into());
        }

        if flags.contains(OpenFlags::O_PATH) {
            let mut file = OpenFile::new(Box::new(path_file::PathFile), flags);
            file.update(target_inode, path.to_owned());
            return Ok(Arc::new(file));
        }
        if attr.file_type == FileType::Symlink {
            return Err(FsError::Loop.into());
        }
        if matches!(attr.file_type, FileType::CharDevice(_))
            && target_inode.mount_flags() & mount::attributes::NODEV != 0
        {
            return Err(FsError::PermissionDenied.into());
        }
        let write_lease = if attr.file_type == FileType::File
            && flags.intersects(OpenFlags::O_WRONLY | OpenFlags::O_RDWR)
        {
            target_inode.begin_write()?
        } else {
            None
        };
        if !created {
            let mut access = match flags & OpenFlags::O_ACCMODE {
                OpenFlags::O_RDONLY => AccessMode::R_OK,
                OpenFlags::O_WRONLY => AccessMode::W_OK,
                OpenFlags::O_RDWR => AccessMode::R_OK | AccessMode::W_OK,
                _ => return Err(KernelError::InvalidValue),
            };
            if flags.contains(OpenFlags::O_TRUNC) {
                access.insert(AccessMode::W_OK);
            }
            creds
                .check_inode_access(target_inode.as_ref(), access)
                .await?;
        }

        if attr.file_type == FileType::Directory
            && (flags.contains(OpenFlags::O_WRONLY) || flags.contains(OpenFlags::O_RDWR))
        {
            return Err(FsError::IsADirectory.into());
        }

        if flags.contains(OpenFlags::O_TRUNC)
            && attr.file_type == FileType::File
            && (flags.contains(OpenFlags::O_WRONLY) || flags.contains(OpenFlags::O_RDWR))
        {
            // Open-time permission checks above include write access for O_TRUNC.
            target_inode.truncate(0).await?;
            notify_modify(target_inode.id()).await;
        }

        match attr.file_type {
            FileType::File => {
                let ops = crate::drivers::fs::proc::open_control(target_inode.as_ref(), &creds)?
                    .or_else(|| crate::drivers::fs::nsfs::open(target_inode.as_ref()))
                    .unwrap_or_else(|| Box::new(RegFile::new(target_inode.inode())));
                let mut open_file = OpenFile::new(ops, flags);
                open_file.retain_write(write_lease);
                open_file.update(target_inode, path.to_owned());

                Ok(Arc::new(open_file))
            }
            FileType::Directory => {
                let mut open_file =
                    OpenFile::new(Box::new(DirFile::new(target_inode.inode())), flags);
                open_file.update(target_inode, path.to_owned());

                Ok(Arc::new(open_file))
            }
            FileType::Symlink => unimplemented!(), // this is implemented at resolve_path_internal
            FileType::BlockDevice(_) => todo!(),
            FileType::CharDevice(char_dev_descriptor) => {
                let char_driver = DM
                    .lock_save_irq()
                    .find_char_driver(char_dev_descriptor.major)
                    .ok_or(FsError::NoDevice)?;

                let mut open_file = char_driver
                    .get_device(char_dev_descriptor.minor)
                    .ok_or(FsError::NoDevice)?
                    .open(flags)?;

                if let Some(of) = Arc::get_mut(&mut open_file) {
                    of.update(target_inode, path.to_owned());
                }

                Ok(open_file)
            }
            FileType::Fifo => todo!(),
            FileType::Socket => todo!(),
        }
    }

    pub async fn mkdir(
        &self,
        path: &Path,
        root: VfsPath,
        mode: FilePermissions,
        task: &Arc<Task>,
    ) -> Result<()> {
        // Try to resolve the target directory first.
        match self.resolve_path(path, root.clone(), task).await {
            // The path already exists, this is an error.
            Ok(_) => Err(FsError::AlreadyExists.into()),

            // The path does not exist, we need to create it.
            Err(KernelError::Fs(FsError::NotFound)) => {
                // Determine the new directory name.
                let dir_name = path.file_name().ok_or(FsError::InvalidInput)?;

                // Resolve the parent directory.  If the path has no parent
                // component (e.g., \"foo\"), treat the provided `root`
                // directory (AT_FDCWD / cwd / dirfd) as the parent.
                let parent_inode = if let Some(parent_path) = path.parent() {
                    self.resolve_path(parent_path, root.clone(), task).await?
                } else {
                    root.clone()
                };

                let parent_attr = parent_inode.getattr().await?;
                if parent_attr.file_type != FileType::Directory {
                    return Err(FsError::NotADirectory.into());
                }
                let creds = task.creds.lock_save_irq().clone();
                creds.check_file_access(&parent_attr, AccessMode::W_OK | AccessMode::X_OK)?;
                let mut mode = FilePermissions::from_bits_truncate(
                    mode.bits() & 0o1777 & !(*task.fs().umask.lock_save_irq() as u16),
                );
                let gid = if parent_attr.permissions.contains(FilePermissions::S_ISGID) {
                    mode.insert(FilePermissions::S_ISGID);
                    parent_attr.gid
                } else {
                    creds.fsgid()
                };
                let _create_lease = parent_inode.begin_write()?;
                let inode = parent_inode
                    .create(dir_name, FileType::Directory, mode, Some(date()))
                    .await?;
                let mut attr = inode.getattr().await?;
                attr.uid = creds.fsuid();
                attr.gid = gid;
                inode.setattr(attr).await?;
                notify_create(parent_inode.id(), dir_name, true).await;

                Ok(())
            }

            // Propagate any other errors up the stack.
            Err(e) => Err(e),
        }
    }

    pub async fn unlink(
        &self,
        path: &Path,
        root: VfsPath,
        remove_dir: bool,
        task: &Arc<Task>,
    ) -> Result<()> {
        // First, resolve the target inode so we can inspect its type.
        let target_inode = self.resolve_path_nofollow(path, root.clone(), task).await?;

        let attr = target_inode.getattr().await?;

        // Validate flag and file-type combinations.
        match attr.file_type {
            FileType::Directory if !remove_dir => {
                return Err(FsError::IsADirectory.into());
            }
            FileType::Directory => { /* OK: rmdir semantics */ }
            _ if remove_dir => {
                return Err(FsError::NotADirectory.into());
            }
            _ => { /* Regular unlink */ }
        }

        // Determine the parent directory inode in which to perform the unlink.
        let parent_inode = if let Some(parent_path) = path.parent() {
            self.resolve_path(parent_path, root.clone(), task).await?
        } else {
            root.clone()
        };

        let parent_attr = parent_inode.getattr().await?;

        // Ensure the parent really is a directory.
        if parent_attr.file_type != FileType::Directory {
            return Err(FsError::NotADirectory.into());
        }

        {
            let creds = task.creds.lock_save_irq();

            creds.check_file_access(&parent_attr, AccessMode::W_OK | AccessMode::X_OK)?;
            creds.check_sticky(&parent_attr, &attr)?;
        }

        // Extract the final component (name) and perform the unlink on the parent.
        let name = path.file_name().ok_or(FsError::InvalidInput)?;

        let _guard = self.path_ops.lock().await;
        if self.is_mountpoint_any(&target_inode) {
            return Err(FsError::Busy.into());
        }
        parent_inode.unlink(name).await?;
        Dentry::unlink(&parent_inode.dentry, name);
        let is_dir = attr.file_type == FileType::Directory;
        notify_delete(parent_inode.id(), name, is_dir).await;
        notify_delete_self(target_inode.id(), is_dir).await;

        Ok(())
    }

    pub async fn link(&self, target: VfsPath, new_parent: VfsPath, name: &str) -> Result<()> {
        // just delegate to inode only, all handling is done at the syscall level
        if target.mount_id() != new_parent.mount_id() {
            return Err(FsError::CrossDevice.into());
        }
        new_parent.link(name, target.inode()).await?;
        notify_create(new_parent.id(), name, false).await;
        Ok(())
    }

    pub async fn symlink(
        &self,
        target: &Path,
        link: &Path,
        root: VfsPath,
        task: &Arc<Task>,
    ) -> Result<()> {
        match self.resolve_path(link, root.clone(), task).await {
            Ok(_) => Err(FsError::AlreadyExists.into()),
            Err(KernelError::Fs(FsError::NotFound)) => {
                let name = link.file_name().ok_or(FsError::InvalidInput)?;

                let parent_inode = if let Some(parent_path) = link.parent() {
                    self.resolve_path(parent_path, root.clone(), task).await?
                } else {
                    root.clone()
                };

                let parent_attr = parent_inode.getattr().await?;
                if parent_attr.file_type != FileType::Directory {
                    return Err(FsError::NotADirectory.into());
                }
                let creds = task.creds.lock_save_irq().clone();
                creds.check_file_access(&parent_attr, AccessMode::W_OK | AccessMode::X_OK)?;
                let _create_lease = parent_inode.begin_write()?;
                parent_inode.symlink(name, target).await?;
                let inode = parent_inode.lookup(name).await?;
                let mut attr = inode.getattr().await?;
                attr.uid = creds.fsuid();
                attr.gid = if parent_attr.permissions.contains(FilePermissions::S_ISGID) {
                    parent_attr.gid
                } else {
                    creds.fsgid()
                };
                inode.setattr(attr).await?;
                notify_create(parent_inode.id(), name, false).await;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub async fn rename(
        &self,
        old_parent_inode: VfsPath,
        old_name: &str,
        new_parent_inode: VfsPath,
        new_name: &str,
        no_replace: bool,
    ) -> Result<()> {
        if old_parent_inode.mount_id() != new_parent_inode.mount_id() {
            return Err(FsError::CrossDevice.into());
        }
        let _guard = self.path_ops.lock().await;
        let target_inode = old_parent_inode.lookup(old_name).await?;
        let target_attr = target_inode.getattr().await?;
        if self.is_mountpoint_any(&target_inode) {
            return Err(FsError::Busy.into());
        }
        if let Ok(victim) = new_parent_inode.lookup(new_name).await
            && self.is_mountpoint_any(&victim)
        {
            return Err(FsError::Busy.into());
        }
        if old_parent_inode == new_parent_inode && old_name == new_name {
            return Ok(());
        }
        // POSIX rename of two hard links to the same inode changes neither
        // directory entry nor the cached paths of their open descriptors.
        if !no_replace
            && new_parent_inode
                .lookup(new_name)
                .await
                .is_ok_and(|p| p.id() == target_inode.id())
        {
            return Ok(());
        }

        new_parent_inode
            .rename_from(old_parent_inode.inode(), old_name, new_name, no_replace)
            .await?;

        Dentry::unlink(&new_parent_inode.dentry, new_name);
        target_inode
            .dentry
            .moved(&new_parent_inode.dentry, new_name);
        notify_move(
            old_parent_inode.id(),
            old_name,
            new_parent_inode.id(),
            new_name,
            target_inode.id(),
            target_attr.file_type == FileType::Directory,
        )
        .await;

        Ok(())
    }

    pub async fn exchange(
        &self,
        old_parent_inode: VfsPath,
        old_name: &str,
        new_parent_inode: VfsPath,
        new_name: &str,
    ) -> Result<()> {
        if old_parent_inode.mount_id() != new_parent_inode.mount_id() {
            return Err(FsError::CrossDevice.into());
        }
        let _guard = self.path_ops.lock().await;
        let old = old_parent_inode.lookup(old_name).await?;
        let new = new_parent_inode.lookup(new_name).await?;
        if self.is_mountpoint_any(&old) || self.is_mountpoint_any(&new) {
            return Err(FsError::Busy.into());
        }
        old_parent_inode
            .exchange(old_name, new_parent_inode.inode(), new_name)
            .await?;
        old.dentry.moved(&new_parent_inode.dentry, new_name);
        new.dentry.moved(&old_parent_inode.dentry, old_name);
        // Restore the new-name cache entry removed when moving the second dentry.
        old.dentry.moved(&new_parent_inode.dentry, new_name);
        Ok(())
    }

    fn is_mountpoint_any(&self, path: &VfsPath) -> bool {
        path.is_mount_root() || path.dentry.mounted.load(Ordering::Acquire) != 0
    }
}

pub static VFS: VFS = VFS::new();

impl VFS {
    /// Flushes all mounted filesystems and their underlying block devices.
    /// Any individual error is logged and ignored so that a single faulty
    /// filesystem does not block the shutdown sequence.
    pub async fn sync_all(&self) -> Result<()> {
        let filesystems: Vec<_> = {
            let state = self.state.lock_save_irq();
            state
                .filesystems
                .values()
                .filter_map(Weak::upgrade)
                .collect()
        };

        for fs in filesystems {
            // Ignore per-filesystem errors; best-effort
            let _ = fs.sync().await;
        }

        Ok(())
    }

    /// Syncs the filesystem that contains the given inode.
    pub async fn sync(&self, inode: Arc<dyn Inode>) -> Result<()> {
        let fs = self
            .state
            .lock_save_irq()
            .get_fs(inode.id())
            .ok_or(FsError::NoDevice)?;
        fs.sync().await
    }
}

#[cfg(test)]
mod tests {
    use crate::fs::VFS;
    use moss_macros::ktest;

    #[ktest]
    async fn test_sync_all() {
        VFS.sync_all().await.unwrap();
    }
}
