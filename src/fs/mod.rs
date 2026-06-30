use crate::clock::realtime::date;
use crate::{
    drivers::{DM, Driver, block::get_block_device_by_descriptor},
    process::{TASK_LIST, Task},
    sync::SpinLock,
};
use alloc::{borrow::ToOwned, boxed::Box, collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use async_trait::async_trait;
use blk::BlockFile;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use dir::DirFile;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{
        BlockDevice, FS_ID_START, FileType, Filesystem, Inode, InodeId, OpenFlags,
        attr::FilePermissions, path::Path,
    },
    proc::caps::CapabilitiesFlags,
};
use open_file::OpenFile;
use reg::RegFile;

pub mod blk;
pub mod dir;
pub mod fops;
pub mod memfd;
pub mod open_file;
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

/// Represents a mounted filesystem.
#[derive(Clone)]
struct Mount {
    fs: Arc<dyn Filesystem>,
    root_inode: Arc<dyn Inode>,
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

/// The internal state of the VFS.
///
/// This struct consolidates the filesystem-wide collections (the list of all
/// registered filesystem instances and the mapping of mount points).
struct VfsState {
    /// A map from an InodeId of a directory to the Mount that is mounted there.
    mounts: BTreeMap<InodeId, Mount>,
    /// A map from a filesystem ID to the corresponding filesystem instance.
    filesystems: BTreeMap<u64, Arc<dyn Filesystem>>,
}

impl VfsState {
    /// Creates a new, empty VfsState.
    const fn new() -> Self {
        Self {
            mounts: BTreeMap::new(),
            filesystems: BTreeMap::new(),
        }
    }

    /// Registers a new filesystem and its mount point.
    fn add_mount(&mut self, mount_point_id: InodeId, mount: Mount) {
        self.filesystems.insert(mount.fs.id(), mount.fs.clone());
        self.mounts.insert(mount_point_id, mount);
    }

    /// Removes a mount point by its inode ID.
    fn remove_mount(&mut self, mount_point_id: &InodeId) -> Option<Mount> {
        let mount = self.mounts.remove(mount_point_id)?;
        self.filesystems.remove(&mount.fs.id())?;
        Some(mount)
    }

    /// Collects the mount identified by `mount_point_id` and every nested mount
    /// reachable beneath it.
    fn collect_mount_subtree(&self, mount_point_id: InodeId) -> Option<Vec<(InodeId, Mount)>> {
        let mut pending = Vec::new();
        let mut subtree = Vec::new();

        pending.push(mount_point_id);

        while let Some(current_mount_point_id) = pending.pop() {
            let mount = self.mounts.get(&current_mount_point_id)?.clone();
            let current_fs_id = mount.fs.id();

            subtree.push((current_mount_point_id, mount));

            for child_mount_point_id in self.mounts.keys().copied() {
                if child_mount_point_id != current_mount_point_id
                    && child_mount_point_id.fs_id() == current_fs_id
                    && !subtree
                        .iter()
                        .any(|(seen, _)| *seen == child_mount_point_id)
                    && !pending.contains(&child_mount_point_id)
                {
                    pending.push(child_mount_point_id);
                }
            }
        }

        Some(subtree)
    }

    /// Checks if an inode is a mount point and returns the root inode of the
    /// mounted filesystem if it is.
    fn get_mount_root(&self, inode_id: &InodeId) -> Option<Arc<dyn Inode>> {
        self.mounts
            .get(inode_id)
            .map(|mount| mount.root_inode.clone())
    }

    fn get_fs(&self, inode_id: InodeId) -> Option<Arc<dyn Filesystem>> {
        self.filesystems.get(&inode_id.fs_id()).cloned()
    }
}

#[allow(clippy::upper_case_acronyms)]
pub struct VFS {
    next_fs_id: AtomicU64,
    state: SpinLock<VfsState>,
    root_inode: SpinLock<Option<Arc<dyn Inode>>>,
}

impl VFS {
    const fn new() -> Self {
        Self {
            next_fs_id: AtomicU64::new(FS_ID_START),
            state: SpinLock::new(VfsState::new()),
            root_inode: SpinLock::new(None),
        }
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

        driver.construct(id, blkdev).await
    }

    /// Mounts the root filesystem.
    pub async fn mount_root(
        &self,
        driver_name: &str,
        blkdev: Option<Box<dyn BlockDevice>>,
    ) -> Result<()> {
        let fs = self.create_fs_instance(driver_name, blkdev).await?;
        let root_inode = fs.root_inode().await?;

        let mount = Mount {
            fs,
            root_inode: root_inode.clone(),
        };

        // Lock the state to add the new mount and filesystem.
        self.state.lock_save_irq().add_mount(root_inode.id(), mount);

        // Set the global root inode.
        *self.root_inode.lock_save_irq() = Some(root_inode);

        Ok(())
    }

    /// Mounts a filesystem at a given directory (mount point).
    pub async fn mount(
        &self,
        mount_point: Arc<dyn Inode>,
        driver_name: &str,
        blkdev: Option<Box<dyn BlockDevice>>,
    ) -> Result<()> {
        if mount_point.getattr().await?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory.into());
        }

        let fs = self.create_fs_instance(driver_name, blkdev).await?;
        let mount_point_id = mount_point.id();
        let root_inode = fs.root_inode().await?;

        let new_mount = Mount { fs, root_inode };

        // Lock the state and insert the new mount.
        self.state
            .lock_save_irq()
            .add_mount(mount_point_id, new_mount);

        Ok(())
    }

    pub async fn umount(&self, mount_point: Arc<dyn Inode>, detach: bool) -> Result<()> {
        let mount_point_id = self
            .mount_point_for_root(mount_point.id())
            .unwrap_or(mount_point.id());

        if mount_point_id
            == self
                .root_inode
                .lock_save_irq()
                .as_ref()
                .ok_or(FsError::NotFound)?
                .id()
        {
            return Err(KernelError::InUse);
        }

        let subtree = {
            let state = self.state.lock_save_irq();
            state
                .collect_mount_subtree(mount_point_id)
                .ok_or(KernelError::InvalidValue)?
        };

        if !detach && subtree.len() > 1 {
            return Err(KernelError::InUse);
        }

        let target_fs_id = subtree
            .first()
            .map(|(_, mount)| mount.fs.id())
            .ok_or(KernelError::InvalidValue)?;

        if !detach {
            let tasks: Vec<_> = TASK_LIST
                .lock_save_irq()
                .values()
                .filter_map(|work| work.upgrade())
                .collect();

            for work in tasks {
                let task = work.task.t_shared.clone();

                if task.root.lock_save_irq().0.id().fs_id() == target_fs_id
                    || task.cwd.lock_save_irq().0.id().fs_id() == target_fs_id
                    || task.fd_table.lock_save_irq().any_inode_on_fs(target_fs_id)
                {
                    return Err(KernelError::InUse);
                }
            }
        }

        let filesystems: Vec<_> = subtree.iter().map(|(_, mount)| mount.fs.clone()).collect();
        for fs in filesystems {
            fs.sync().await?;
        }

        let mut state = self.state.lock_save_irq();
        for (mount_point_id, _) in subtree {
            state
                .remove_mount(&mount_point_id)
                .ok_or(KernelError::InvalidValue)?;
        }

        Ok(())
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
        root: Arc<dyn Inode>,
        task: &Arc<Task>,
    ) -> Result<Arc<dyn Inode>> {
        let root = if path.is_absolute() {
            task.root.lock_save_irq().0.clone() // use the task's root inode, in case a custom chroot was set
        } else {
            root
        };

        self.resolve_path_internal(path, root, true).await
    }

    /// Resolves a path string to an Inode, starting from a given root for
    /// relative paths, without following the final symbolic link.
    pub async fn resolve_path_nofollow(
        &self,
        path: &Path,
        root: Arc<dyn Inode>,
        task: &Arc<Task>,
    ) -> Result<Arc<dyn Inode>> {
        let root = if path.is_absolute() {
            task.root.lock_save_irq().0.clone()
        } else {
            root
        };

        self.resolve_path_internal(path, root, false).await
    }

    /// Resolves a path string to an Inode, starting from a given root for
    /// relative paths, and using the filesystem root inode for absolute paths.
    pub async fn resolve_path_absolute(
        &self,
        path: &Path,
        root: Arc<dyn Inode>,
    ) -> Result<Arc<dyn Inode>> {
        let root = if path.is_absolute() {
            self.root_inode
                .lock_save_irq()
                .as_ref()
                .cloned()
                .ok_or(FsError::NotFound)?
        } else {
            root
        };

        self.resolve_path_internal(path, root, true).await
    }

    async fn resolve_path_internal(
        &self,
        path: &Path,
        root: Arc<dyn Inode>,
        follow_last_sym: bool,
    ) -> Result<Arc<dyn Inode>> {
        let mut current_inode = root;
        let mut symlink_count = 0;

        let mut components: Vec<_> = path.components().map(|s| s.to_owned()).collect();
        components.reverse();

        while let Some(component) = components.pop() {
            // Before looking up the component, check if the current inode is a
            // mount point. If so, traverse into the mounted filesystem's root.
            if let Some(mount_root) = self
                .state
                .lock_save_irq()
                .get_mount_root(&current_inode.id())
            {
                current_inode = mount_root;
            }

            let next_inode = current_inode.lookup(&component).await?;

            let attr = next_inode.getattr().await?;

            if attr.file_type == FileType::Symlink && (follow_last_sym || !components.is_empty()) {
                symlink_count += 1;
                if symlink_count > MAX_SYMLINK {
                    return Err(FsError::Loop.into()); // prevent infinite looping
                }

                let target = next_inode.readlink().await?;
                let mut new_components: Vec<_> =
                    target.components().map(|s| s.to_owned()).collect();
                new_components.reverse();
                for comp in new_components {
                    components.push(comp);
                }

                if target.is_absolute() {
                    // if absolute, restart from root
                    current_inode = self.root_inode.lock_save_irq().as_ref().unwrap().clone();
                }

                continue;
            }

            // Delegate the lookup to the underlying filesystem.
            current_inode = next_inode;
        }

        // After the final lookup, check if the destination is itself a mount point.
        if let Some(mount_root) = self
            .state
            .lock_save_irq()
            .get_mount_root(&current_inode.id())
        {
            current_inode = mount_root;
        }

        Ok(current_inode)
    }

    /// Returns a clone of the root inode.
    pub fn root_inode(&self) -> Arc<dyn Inode> {
        self.root_inode.lock_save_irq().as_ref().unwrap().clone()
    }

    pub async fn open(
        &self,
        path: &Path,
        flags: OpenFlags,
        root: Arc<dyn Inode>,
        mode: FilePermissions,
        task: &Arc<Task>,
    ) -> Result<Arc<OpenFile>> {
        // Attempt to resolve the full path first.
        let resolve_result = self.resolve_path(path, root.clone(), task).await;

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
                    if parent_inode.getattr().await?.file_type != FileType::Directory {
                        return Err(FsError::NotADirectory.into());
                    }

                    parent_inode
                        .create(file_name, FileType::File, mode, Some(date()))
                        .await?
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

        if attr.file_type == FileType::Directory
            && (flags.contains(OpenFlags::O_WRONLY) || flags.contains(OpenFlags::O_RDWR))
        {
            return Err(FsError::IsADirectory.into());
        }

        if flags.contains(OpenFlags::O_TRUNC)
            && attr.file_type == FileType::File
            && (flags.contains(OpenFlags::O_WRONLY) || flags.contains(OpenFlags::O_RDWR))
        {
            // TODO: Check for write permissions on the inode itself.
            target_inode.truncate(0).await?;
        }

        match attr.file_type {
            FileType::File => {
                let mut open_file =
                    OpenFile::new(Box::new(RegFile::new(target_inode.clone())), flags);
                open_file.update(target_inode, path.to_owned());

                Ok(Arc::new(open_file))
            }
            FileType::Directory => {
                let mut open_file =
                    OpenFile::new(Box::new(DirFile::new(target_inode.clone())), flags);
                open_file.update(target_inode, path.to_owned());

                Ok(Arc::new(open_file))
            }
            FileType::Symlink => unimplemented!(), // this is implemented at resolve_path_internal
            FileType::BlockDevice(block_dev_descriptor) => {
                let (_, block_device) = get_block_device_by_descriptor(block_dev_descriptor)
                    .ok_or(FsError::NoDevice)?;

                let mut open_file = OpenFile::new(Box::new(BlockFile::new(block_device)), flags);
                open_file.update(target_inode, path.to_owned());

                Ok(Arc::new(open_file))
            }
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
        root: Arc<dyn Inode>,
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

                // Verify that the parent is actually a directory.
                if parent_inode.getattr().await?.file_type != FileType::Directory {
                    return Err(FsError::NotADirectory.into());
                }

                // Delegate the creation to the filesystem-specific inode.
                parent_inode
                    .create(dir_name, FileType::Directory, mode, Some(date()))
                    .await?;

                Ok(())
            }

            // Propagate any other errors up the stack.
            Err(e) => Err(e),
        }
    }

    pub async fn unlink(
        &self,
        path: &Path,
        root: Arc<dyn Inode>,
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

            if attr.permissions.contains(FilePermissions::S_ISVTX)
                && attr.uid != creds.euid()
                && parent_attr.uid != creds.euid()
            {
                creds.caps().check_capable(CapabilitiesFlags::CAP_FOWNER)?;
            }
        }

        // Extract the final component (name) and perform the unlink on the parent.
        let name = path.file_name().ok_or(FsError::InvalidInput)?;

        parent_inode.unlink(name).await?;

        Ok(())
    }

    pub async fn link(
        &self,
        target: Arc<dyn Inode>,
        new_parent: Arc<dyn Inode>,
        name: &str,
    ) -> Result<()> {
        // just delegate to inode only, all handling is done at the syscall level
        new_parent.link(name, target).await
    }

    pub async fn symlink(
        &self,
        target: &Path,
        link: &Path,
        root: Arc<dyn Inode>,
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

                // verify that the parent inode is a directory
                if parent_inode.getattr().await?.file_type != FileType::Directory {
                    return Err(FsError::NotADirectory.into());
                }

                parent_inode.symlink(name, target).await
            }
            Err(e) => Err(e),
        }
    }

    pub async fn rename(
        &self,
        old_parent_inode: Arc<dyn Inode>,
        old_name: &str,
        new_parent_inode: Arc<dyn Inode>,
        new_name: &str,
        no_replace: bool,
    ) -> Result<()> {
        new_parent_inode
            .rename_from(old_parent_inode, old_name, new_name, no_replace)
            .await
    }

    pub async fn exchange(
        &self,
        old_parent_inode: Arc<dyn Inode>,
        old_name: &str,
        new_parent_inode: Arc<dyn Inode>,
        new_name: &str,
    ) -> Result<()> {
        old_parent_inode
            .exchange(old_name, new_parent_inode, new_name)
            .await
    }

    pub fn is_mount_root(&self, id: InodeId) -> bool {
        self.state
            .lock_save_irq()
            .mounts
            .values()
            .any(|mount| mount.root_inode.id() == id)
    }

    pub fn is_mount_point(&self, id: InodeId) -> bool {
        self.state.lock_save_irq().mounts.contains_key(&id)
    }

    pub fn mount_point_for_root(&self, id: InodeId) -> Option<InodeId> {
        self.state
            .lock_save_irq()
            .mounts
            .iter()
            .find_map(|(mount_point, mount)| (mount.root_inode.id() == id).then_some(*mount_point))
    }

    pub fn pivot_root(
        &self,
        new_root_mount_point: InodeId,
        put_old_mount_point: InodeId,
    ) -> Result<(Arc<dyn Inode>, Arc<dyn Inode>)> {
        let old_root_inode = self
            .root_inode
            .lock_save_irq()
            .as_ref()
            .cloned()
            .ok_or(FsError::NotFound)?;

        let (old_root_root, new_root_root) = {
            let mut state = self.state.lock_save_irq();

            if state.mounts.contains_key(&put_old_mount_point) {
                return Err(KernelError::InUse);
            }

            let old_root_mount = state
                .mounts
                .remove(&old_root_inode.id())
                .ok_or(FsError::NotFound)?;
            let new_root_mount = state
                .mounts
                .remove(&new_root_mount_point)
                .ok_or(FsError::InvalidInput)?;

            let old_root_root = old_root_mount.root_inode.clone();
            let new_root_root = new_root_mount.root_inode.clone();

            state.mounts.insert(new_root_root.id(), new_root_mount);
            state.mounts.insert(put_old_mount_point, old_root_mount);

            (old_root_root, new_root_root)
        };

        *self.root_inode.lock_save_irq() = Some(new_root_root.clone());

        Ok((old_root_root, new_root_root))
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
            state.filesystems.values().cloned().collect()
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
