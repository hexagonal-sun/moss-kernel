//! EXT4 Filesystem Driver

#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

use crate::error::FsError;
use crate::fs::path::Path;
use crate::fs::pathbuf::PathBuf;
use crate::fs::{DirStream, Dirent};
use crate::proc::ids::{Gid, Uid};
use crate::sync::mutex::Mutex;
use crate::{
    CpuOps,
    error::{KernelError, Result},
    fs::{
        FileType, Filesystem, Inode, InodeId,
        attr::{FileAttr, FilePermissions},
        blk::buffer::BlockBuffer,
    },
};
use alloc::string::ToString;
use alloc::vec::Vec;
use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use async_trait::async_trait;
use core::any::Any;
use core::error::Error;
use core::marker::PhantomData;
use core::num::NonZeroU32;
use core::ops::{Deref, DerefMut};
use core::time::Duration;
use ext4plus::prelude::{
    AsyncIterator, AsyncSkip, Dir, DirEntryName, Ext4, Ext4Error, Ext4Read, Ext4Write, File,
    FollowSymlinks, Inode as ExtInode, InodeCreationOptions, InodeFlags, InodeMode, Metadata,
    PathBuf as ExtPathBuf, ReadDir, write_at,
};
use log::error;

#[async_trait]
impl Ext4Read for BlockBuffer {
    async fn read(
        &self,
        start_byte: u64,
        dst: &mut [u8],
    ) -> core::result::Result<(), Box<dyn Error + Send + Sync + 'static>> {
        Ok(self.read_at(start_byte, dst).await?)
    }
}

#[async_trait]
impl Ext4Write for BlockBuffer {
    async fn write(
        &self,
        start_byte: u64,
        src: &[u8],
    ) -> core::result::Result<(), Box<dyn Error + Send + Sync + 'static>> {
        Ok(self.write_at(start_byte, src).await?)
    }
}

impl From<Ext4Error> for KernelError {
    fn from(err: Ext4Error) -> Self {
        match err {
            Ext4Error::NotFound => KernelError::Fs(FsError::NotFound),
            Ext4Error::NotADirectory => KernelError::Fs(FsError::NotADirectory),
            Ext4Error::AlreadyExists => KernelError::Fs(FsError::AlreadyExists),
            Ext4Error::Corrupt(c) => {
                error!("Corrupt EXT4 filesystem: {c}, likely a bug");
                KernelError::Fs(FsError::InvalidFs)
            }
            e => {
                error!("Unmapped EXT4 error: {e:?}");
                KernelError::Other("EXT4 error")
            }
        }
    }
}

impl From<ext4plus::FileType> for FileType {
    fn from(ft: ext4plus::FileType) -> Self {
        match ft {
            ext4plus::FileType::BlockDevice => todo!(),
            ext4plus::FileType::CharacterDevice => todo!(),
            ext4plus::FileType::Directory => FileType::Directory,
            ext4plus::FileType::Fifo => FileType::Fifo,
            ext4plus::FileType::Regular => FileType::File,
            ext4plus::FileType::Socket => FileType::Socket,
            ext4plus::FileType::Symlink => FileType::Symlink,
        }
    }
}

impl From<Metadata> for FileAttr {
    fn from(meta: Metadata) -> Self {
        FileAttr {
            size: meta.size_in_bytes,
            file_type: meta.file_type.into(),
            permissions: FilePermissions::from_bits_truncate(meta.mode.bits()),
            uid: Uid::new(meta.uid),
            gid: Gid::new(meta.gid),
            atime: meta.atime,
            ctime: meta.ctime,
            mtime: meta.mtime,
            nlinks: meta.links_count as u32,
            ..Default::default()
        }
    }
}

pub struct ReadDirWrapper {
    inner: AsyncSkip<ReadDir>,
    fs_id: u64,
    current_off: u64,
}

impl ReadDirWrapper {
    pub fn new(inner: ReadDir, fs_id: u64, start_offset: u64) -> Self {
        Self {
            inner: inner.skip(start_offset as usize),
            fs_id,
            current_off: start_offset,
        }
    }
}

#[async_trait]
impl DirStream for ReadDirWrapper {
    async fn next_entry(&mut self) -> Result<Option<Dirent>> {
        match self.inner.next().await {
            Some(entry) => {
                let entry = entry?;
                self.current_off += 1;
                Ok(Some(Dirent {
                    id: InodeId::from_fsid_and_inodeid(self.fs_id, entry.inode.get() as u64),
                    name: entry.file_name().as_str().unwrap().to_string(),
                    file_type: entry.file_type()?.into(),
                    offset: self.current_off,
                }))
            }
            None => Ok(None),
        }
    }
}

enum InodeInner {
    Regular(File),
    Directory(Dir),
    Other(ExtInode),
}

impl InodeInner {
    async fn new(inode: ExtInode, fs: &Ext4) -> Self {
        match inode.file_type() {
            ext4plus::FileType::Regular => {
                InodeInner::Regular(File::open_inode(fs, inode).unwrap())
            }
            ext4plus::FileType::Directory => {
                InodeInner::Directory(Dir::open_inode(fs, inode).unwrap())
            }
            _ => InodeInner::Other(inode),
        }
    }
}

impl Deref for InodeInner {
    type Target = ExtInode;

    fn deref(&self) -> &Self::Target {
        match self {
            InodeInner::Regular(f) => f.inode(),
            InodeInner::Directory(d) => d.inode(),
            InodeInner::Other(i) => i,
        }
    }
}

impl DerefMut for InodeInner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            InodeInner::Regular(f) => f.inode_mut(),
            InodeInner::Directory(d) => d.inode_mut(),
            InodeInner::Other(i) => i,
        }
    }
}

pub struct Ext4Inode<CPU: CpuOps> {
    fs_ref: Weak<Ext4Filesystem<CPU>>,
    id: NonZeroU32,
    inner: Mutex<InodeInner, CPU>,
    path: ExtPathBuf,
}

#[async_trait]
impl<CPU> Inode for Ext4Inode<CPU>
where
    CPU: CpuOps + Send + Sync,
{
    fn id(&self) -> InodeId {
        let fs = self.fs_ref.upgrade().unwrap();
        InodeId::from_fsid_and_inodeid(fs.id(), self.id.get() as u64)
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let inner = self.inner.lock().await;
        // Must be a regular file.
        if inner.file_type() != ext4plus::FileType::Regular {
            return Err(KernelError::NotSupported);
        }

        let file_size = inner.size_in_bytes();

        // Past EOF = nothing to read.
        if offset >= file_size {
            return Ok(0);
        }

        // Do not read past the end of the file.
        let to_read = core::cmp::min(buf.len() as u64, file_size - offset) as usize;

        let fs = self.fs_ref.upgrade().unwrap();
        let mut file = File::open_inode(&fs.inner, inner.clone())?;

        file.seek_to(offset).await?;

        // `ext4plus::File::read_bytes` may return fewer bytes than requested
        // if the read crosses a block boundary. Loop until we've filled
        // `to_read` bytes or hit EOF.
        let mut total_read = 0;
        while total_read < to_read {
            let bytes_read = file.read_bytes(&mut buf[total_read..to_read]).await?;
            if bytes_read == 0 {
                break; // EOF
            }
            total_read += bytes_read;
        }

        Ok(total_read)
    }

    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize> {
        let mut inner = self.inner.lock().await;
        // Must be a regular file.
        if inner.file_type() != ext4plus::FileType::Regular {
            return Err(KernelError::NotSupported);
        }

        let fs = self.fs_ref.upgrade().unwrap();
        let total_written = write_at(&fs.inner, &mut inner, buf, offset).await?;

        Ok(total_written)
    }

    async fn truncate(&self, size: u64) -> Result<()> {
        let inner = self.inner.lock().await;
        if inner.file_type() != ext4plus::FileType::Regular {
            return Err(KernelError::NotSupported);
        }
        let fs = self.fs_ref.upgrade().unwrap();
        let mut file = File::open_inode(&fs.inner, inner.clone())?;
        file.truncate(size).await?;
        Ok(())
    }

    async fn getattr(&self) -> Result<FileAttr> {
        let inner = self.inner.lock().await;
        let mut attrs: FileAttr = inner.metadata().into();
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;

        attrs.id = InodeId::from_fsid_and_inodeid(fs.id(), self.id.get() as u64);

        Ok(attrs)
    }

    async fn setattr(&self, attr: FileAttr) -> Result<()> {
        let mut inner = self.inner.lock().await;
        inner.set_atime(attr.atime);
        inner.set_ctime(attr.ctime);
        inner.set_mtime(attr.mtime);
        inner.set_gid(attr.gid.into());
        inner.set_uid(attr.uid.into());
        inner.set_links_count(attr.nlinks as u16);
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;
        inner.write(&fs.inner).await?;
        Ok(())
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        let fs = self.fs_ref.upgrade().unwrap();
        let inner = self.inner.lock().await;
        let child_inode = match &*inner {
            InodeInner::Directory(d) => {
                d.get_entry(DirEntryName::try_from(name.as_bytes()).unwrap())
                    .await?
            }
            _ => return Err(KernelError::NotSupported),
        };
        let child_path = self.path.join(name);
        Ok(Arc::new(Ext4Inode::<CPU> {
            fs_ref: self.fs_ref.clone(),
            id: child_inode.index,
            inner: Mutex::new(InodeInner::new(child_inode, &fs.inner).await),
            path: child_path,
        }))
    }

    async fn create(
        &self,
        name: &str,
        file_type: FileType,
        permissions: FilePermissions,
        time: Option<Duration>,
    ) -> Result<Arc<dyn Inode>> {
        let fs = self.fs_ref.upgrade().unwrap();
        let mut inner = self.inner.lock().await;
        let inner_dir = match &mut *inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };
        let mut new_inode = if matches!(file_type, FileType::File) {
            let inode = fs
                .inner
                .create_inode(InodeCreationOptions {
                    file_type: ext4plus::FileType::Regular,
                    mode: InodeMode::S_IFREG | InodeMode::from_bits(permissions.bits()).unwrap(),
                    uid: 0,
                    gid: 0,
                    time: time.unwrap_or_default(),
                    flags: InodeFlags::empty(),
                })
                .await?;
            InodeInner::Regular(File::open_inode(&fs.inner, inode)?)
        } else if matches!(file_type, FileType::Directory) {
            let old_links_count = inner_dir.inode().links_count();
            inner_dir.inode_mut().set_links_count(old_links_count + 1);
            let inode = fs
                .inner
                .create_inode(InodeCreationOptions {
                    file_type: ext4plus::FileType::Directory,
                    mode: InodeMode::S_IFDIR | InodeMode::from_bits(permissions.bits()).unwrap(),
                    uid: 0,
                    gid: 0,
                    time: Default::default(),
                    flags: InodeFlags::empty(),
                })
                .await?;
            let dir = Dir::init(fs.inner.clone(), inode, self.id).await?;
            InodeInner::Directory(dir)
        } else {
            return Err(KernelError::NotSupported);
        };
        inner_dir
            .link(
                DirEntryName::try_from(name.as_bytes()).unwrap(),
                new_inode.deref_mut(),
            )
            .await?;
        let new_path = self.path.join(name);
        Ok(Arc::new(Ext4Inode::<CPU> {
            fs_ref: self.fs_ref.clone(),
            id: new_inode.index,
            inner: Mutex::new(new_inode),
            path: new_path,
        }))
    }

    async fn link(&self, name: &str, inode: Arc<dyn Inode>) -> Result<()> {
        let inner = self.inner.lock().await;
        let inner_dir = match &*inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };
        let fs = self.fs_ref.upgrade().unwrap();
        // TODO: This forces the other inode out of sync
        // Check fs ids match
        if inode.id().fs_id() != fs.id() {
            return Err(KernelError::Fs(FsError::CrossDevice));
        }
        let mut other_inode = inode
            .as_any()
            .downcast_ref::<Ext4Inode<CPU>>()
            .ok_or(FsError::CrossDevice)?
            .inner
            .lock()
            .await;
        let file_type = other_inode.file_type();
        inner_dir
            .link(
                DirEntryName::try_from(name.as_bytes()).unwrap(),
                &mut other_inode,
            )
            .await?;
        Ok(())
    }

    async fn unlink(&self, name: &str) -> Result<()> {
        let inner = self.inner.lock().await;
        let inner_dir = match &*inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };
        let fs = self.fs_ref.upgrade().unwrap();
        let entry = DirEntryName::try_from(name.as_bytes()).unwrap();
        let child_inode = inner_dir.get_entry(entry).await?;
        inner_dir.unlink(entry, child_inode).await?;
        Ok(())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let inner = self.inner.lock().await;
        if inner.file_type() != ext4plus::FileType::Directory {
            return Err(KernelError::NotSupported);
        }
        let fs = self.fs_ref.upgrade().unwrap();
        Ok(Box::new(ReadDirWrapper::new(
            ReadDir::new(fs.inner.clone(), &inner, self.path.clone())?,
            fs.id(),
            start_offset,
        )))
    }

    async fn readlink(&self) -> Result<PathBuf> {
        let inner = self.inner.lock().await;
        if inner.file_type() != ext4plus::FileType::Symlink {
            return Err(KernelError::NotSupported);
        }
        let fs = self.fs_ref.upgrade().unwrap();
        // Conversion has to ensure path is valid UTF-8 (O(n) time).
        Ok(inner
            .symlink_target(&fs.inner)
            .await
            .map(|p| PathBuf::from(p.to_str().unwrap()))?)
    }

    async fn rename_from(
        &self,
        old_parent: Arc<dyn Inode>,
        old_name: &str,
        new_name: &str,
        no_replace: bool,
    ) -> Result<()> {
        if old_name == new_name && old_parent.id().inode_id() == self.id().inode_id() {
            return Ok(());
        }

        if old_parent.id().fs_id() != self.id().fs_id() {
            return Err(KernelError::Fs(FsError::CrossDevice));
        }
        let fs = self.fs_ref.upgrade().unwrap();

        let old_parent_inode = old_parent
            .as_any()
            .downcast_ref::<Ext4Inode<CPU>>()
            .ok_or(FsError::CrossDevice)?
            .inner
            .lock()
            .await
            .clone();
        let old_parent_dir = Dir::open_inode(&fs.inner, old_parent_inode)?;

        let inner = self.inner.lock().await;
        let inner_dir = match &*inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };

        // inode being moved (source)
        let mut child_inode = old_parent_dir
            .get_entry(
                DirEntryName::try_from(old_name)
                    .map_err(|_| KernelError::Fs(FsError::InvalidInput))?,
            )
            .await?;

        // Check if destination exists and handle overwrite constraints.
        let dst_lookup = inner_dir
            .get_entry(
                DirEntryName::try_from(new_name)
                    .map_err(|_| KernelError::Fs(FsError::InvalidInput))?,
            )
            .await;

        if no_replace && dst_lookup.is_ok() {
            return Err(KernelError::Fs(FsError::AlreadyExists));
        }

        if let Ok(target_inode) = dst_lookup {
            let target_kind = target_inode.file_type();
            let source_kind = child_inode.file_type();

            if target_kind == ext4plus::FileType::Directory {
                let target_is_empty = fs
                    .inner
                    .read_dir(&self.path.join(new_name))
                    .await?
                    .all(|e| {
                        let Ok(entry) = e else {
                            // If we fail to read the directory, be conservative and treat it as non-empty.
                            return false;
                        };
                        let name = entry.file_name().as_str().unwrap();
                        name == "." || name == ".."
                    })
                    .await;

                if !target_is_empty {
                    return Err(KernelError::Fs(FsError::DirectoryNotEmpty));
                }
                if source_kind != ext4plus::FileType::Directory {
                    return Err(KernelError::Fs(FsError::IsADirectory));
                }
            } else if source_kind == ext4plus::FileType::Directory {
                // Can't replace non-directory with a directory.
                return Err(KernelError::Fs(FsError::NotADirectory));
            }

            // Overwrite: remove destination entry first.
            inner_dir
                .unlink(
                    DirEntryName::try_from(new_name)
                        .map_err(|_| KernelError::Fs(FsError::InvalidInput))?,
                    target_inode,
                )
                .await?;
        }

        // Link into destination parent, then unlink from source parent.
        inner_dir
            .link(
                DirEntryName::try_from(new_name.as_bytes()).unwrap(),
                &mut child_inode,
            )
            .await?;
        old_parent_dir
            .unlink(
                DirEntryName::try_from(old_name.as_bytes()).unwrap(),
                child_inode,
            )
            .await?;
        *old_parent
            .as_any()
            .downcast_ref::<Ext4Inode<CPU>>()
            .ok_or(FsError::CrossDevice)?
            .inner
            .lock()
            .await = InodeInner::Directory(old_parent_dir);
        Ok(())
    }

    async fn symlink(&self, name: &str, target: &Path) -> Result<()> {
        let inner = self.inner.lock().await;
        let inner_dir = match &*inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };
        if inner.file_type() != ext4plus::FileType::Directory {
            return Err(KernelError::NotSupported);
        }
        let fs = self.fs_ref.upgrade().unwrap();
        fs.inner
            .symlink(
                inner_dir,
                DirEntryName::try_from(name.as_bytes()).unwrap(),
                ExtPathBuf::new(target.as_str().as_bytes()),
                0,
                0,
                Duration::from_secs(0),
            )
            .await?;
        Ok(())
    }

    async fn sync(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;
        inner.write(&fs.inner).await?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An EXT4 filesystem instance.
///
/// For now this struct only stores the underlying block buffer and an ID
/// assigned by the VFS when the filesystem is mounted.
pub struct Ext4Filesystem<CPU: CpuOps> {
    inner: Ext4,
    id: u64,
    this: Weak<Ext4Filesystem<CPU>>,
    dev: Arc<BlockBuffer>,
    _phantom_data: PhantomData<CPU>,
}

impl<CPU> Ext4Filesystem<CPU>
where
    CPU: CpuOps + Send + Sync,
{
    /// Construct a new EXT4 filesystem instance.
    pub async fn new(dev: BlockBuffer, id: u64) -> Result<Arc<Self>> {
        let dev_arc = Arc::new(dev);
        let inner =
            Ext4::load_with_writer(Box::new(dev_arc.clone()), Some(Box::new(dev_arc.clone())))
                .await?;
        Ok(Arc::new_cyclic(|weak| Self {
            inner,
            id,
            this: weak.clone(),
            dev: dev_arc,
            _phantom_data: PhantomData,
        }))
    }
}

#[async_trait]
impl<CPU> Filesystem for Ext4Filesystem<CPU>
where
    CPU: CpuOps + Send + Sync,
{
    fn id(&self) -> u64 {
        self.id
    }

    fn magic(&self) -> u64 {
        // TODO: retrieve magic from superblock instead of hardcoding
        0xef53 // EXT4 magic number
    }

    /// Returns the root inode of the mounted EXT4 filesystem.
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        let root = self.inner.read_root_inode().await?;
        Ok(Arc::new(Ext4Inode::<CPU> {
            fs_ref: self.this.clone(),
            id: root.index,
            inner: Mutex::new(InodeInner::new(root, &self.inner).await),
            path: ExtPathBuf::new("/"),
        }))
    }

    /// Flushes any dirty data to the underlying block device.  The current
    /// stub implementation simply forwards the request to `BlockBuffer::sync`.
    async fn sync(&self) -> Result<()> {
        self.dev.sync().await?;
        Ok(())
    }
}
