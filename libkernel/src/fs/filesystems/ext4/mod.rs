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
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
    vec,
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
    PathBuf as ExtPathBuf, ReadDir, read_at, write_at,
};
use log::error;

mod cache;
mod disk;
#[cfg(test)]
mod tests;
use cache::InodeCache;
use disk::DiskLayout;

// BlockBuffer performs sector read/modify/write. Serialize those writes even
// for distinct inodes, which may occupy the same device sector.
struct Ext4Device<CPU: CpuOps> {
    buffer: BlockBuffer,
    writes: Mutex<(), CPU>,
}

impl<CPU: CpuOps> Ext4Device<CPU> {
    async fn read_at(&self, offset: u64, bytes: &mut [u8]) -> Result<()> {
        self.buffer.read_at(offset, bytes).await
    }

    async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<()> {
        let _write = self.writes.lock().await;
        self.buffer.write_at(offset, bytes).await
    }
}

#[async_trait]
impl<CPU: CpuOps + Send + Sync> Ext4Read for Ext4Device<CPU> {
    async fn read(
        &self,
        start_byte: u64,
        dst: &mut [u8],
    ) -> core::result::Result<(), Box<dyn Error + Send + Sync + 'static>> {
        Ok(self.read_at(start_byte, dst).await?)
    }
}

#[async_trait]
impl<CPU: CpuOps + Send + Sync> Ext4Write for Ext4Device<CPU> {
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

/// Wraps an ext4 directory iterator to produce VFS [`Dirent`] entries.
pub struct ReadDirWrapper {
    inner: AsyncSkip<ReadDir>,
    fs_id: u64,
    current_off: u64,
}

impl ReadDirWrapper {
    /// Creates a new `ReadDirWrapper` starting at the given offset.
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

/// An inode within an ext4 filesystem.
pub struct Ext4Inode<CPU: CpuOps> {
    fs_ref: Weak<Ext4Filesystem<CPU>>,
    id: NonZeroU32,
    inner: Arc<Mutex<InodeInner, CPU>>,
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
        let mut inner = self.inner.lock().await;
        if inner.file_type() != ext4plus::FileType::Regular {
            return Err(KernelError::NotSupported);
        }
        let fs = self.fs_ref.upgrade().unwrap();
        // The library frees whole blocks but leaves the retained EOF block's
        // tail unchanged. Clear it before shrinking, without allocating holes.
        if size < inner.size_in_bytes() && size % fs.disk.block_size != 0 {
            let end = size
                .checked_add(fs.disk.block_size - size % fs.disk.block_size)
                .ok_or(KernelError::TooLarge)?;
            let mut view = inner.clone();
            view.set_size_in_bytes(end);
            let mut tail = vec![0; (end - size) as usize];
            let read = read_at(&fs.inner, &view, &mut tail, size).await?;
            if read != tail.len() {
                return Err(KernelError::Other("short truncate tail read"));
            }
            if tail.iter().any(|&byte| byte != 0) {
                tail.fill(0);
                if write_at(&fs.inner, &mut inner, &tail, size).await? != tail.len() {
                    return Err(KernelError::Other("short truncate tail write"));
                }
            }
        }
        let mut file = File::open_inode(&fs.inner, inner.clone())?;
        file.truncate(size).await?;
        *inner = InodeInner::Regular(file);
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
        let _namespace = fs.namespace.lock().await;
        let inner = self.inner.lock().await;
        let child_inode = match &*inner {
            InodeInner::Directory(d) => {
                d.get_entry(DirEntryName::try_from(name.as_bytes()).unwrap())
                    .await?
            }
            _ => return Err(KernelError::NotSupported),
        };
        let child_path = self.path.join(name);
        let child_id = child_inode.index;
        let child_inner = fs
            .shared_inode_inner(InodeInner::new(child_inode, &fs.inner).await)
            .await?;
        Ok(Arc::new(Ext4Inode::<CPU> {
            fs_ref: self.fs_ref.clone(),
            id: child_id,
            inner: child_inner,
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
        let _namespace = fs.namespace.lock().await;
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
            // Dir::link accounts for the child's ".." link to this parent.
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
        let new_inode_id = new_inode.index;
        let new_inode_inner = fs.shared_inode_inner(new_inode).await?;
        Ok(Arc::new(Ext4Inode::<CPU> {
            fs_ref: self.fs_ref.clone(),
            id: new_inode_id,
            inner: new_inode_inner,
            path: new_path,
        }))
    }

    async fn link(&self, name: &str, inode: Arc<dyn Inode>) -> Result<()> {
        let fs = self.fs_ref.upgrade().unwrap();
        let _namespace = fs.namespace.lock().await;
        if inode.id().fs_id() != fs.id() {
            return Err(KernelError::Fs(FsError::CrossDevice));
        }
        if inode.id() == self.id() {
            return Err(KernelError::Fs(FsError::IsADirectory));
        }
        let mut inner = self.inner.lock().await;
        let inner_dir = match &mut *inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };
        let mut other_inode = inode
            .as_any()
            .downcast_ref::<Ext4Inode<CPU>>()
            .ok_or(FsError::CrossDevice)?
            .inner
            .lock()
            .await;
        if other_inode.file_type() == ext4plus::FileType::Directory {
            return Err(KernelError::Fs(FsError::IsADirectory));
        }
        inner_dir
            .link(
                DirEntryName::try_from(name.as_bytes()).unwrap(),
                &mut other_inode,
            )
            .await?;
        Ok(())
    }

    async fn unlink(&self, name: &str) -> Result<()> {
        let fs = self.fs_ref.upgrade().unwrap();
        let _namespace = fs.namespace.lock().await;
        let mut inner = self.inner.lock().await;
        let inner_dir = match &mut *inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };
        fs.unlink_locked(inner_dir, name).await
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
        if old_parent.id().fs_id() != self.id().fs_id() {
            return Err(KernelError::Fs(FsError::CrossDevice));
        }
        let fs = self.fs_ref.upgrade().unwrap();
        let _namespace = fs.namespace.lock().await;
        let old_parent = old_parent
            .as_any()
            .downcast_ref::<Ext4Inode<CPU>>()
            .ok_or(FsError::CrossDevice)?;
        let mut inner = self.inner.lock().await;
        let inner_dir = match &mut *inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };
        if old_parent.id == self.id {
            // Both wrappers share one mutex and one directory object.
            fs.rename_locked(None, inner_dir, old_name, new_name, no_replace)
                .await
        } else {
            // namespace serializes multi-inode operations, so opposing moves
            // cannot acquire these directory locks in opposite orders.
            let mut old_inner = old_parent.inner.lock().await;
            let InodeInner::Directory(old_dir) = &mut *old_inner else {
                return Err(KernelError::Fs(FsError::NotADirectory));
            };
            fs.rename_locked(Some(old_dir), inner_dir, old_name, new_name, no_replace)
                .await
        }
    }

    async fn symlink(&self, name: &str, target: &Path) -> Result<()> {
        let fs = self.fs_ref.upgrade().unwrap();
        let _namespace = fs.namespace.lock().await;
        let mut inner = self.inner.lock().await;
        let inner_dir = match &mut *inner {
            InodeInner::Directory(d) => d,
            _ => return Err(KernelError::NotSupported),
        };
        let entry = Ext4Filesystem::<CPU>::entry_name(name)?;
        match inner_dir.get_entry(entry).await {
            Ok(_) => return Err(FsError::AlreadyExists.into()),
            Err(Ext4Error::NotFound) => (),
            Err(error) => return Err(error.into()),
        }
        if target.as_str().len() == 60 {
            // ext4 fast symlinks must leave a byte for the terminator. The
            // library's creation threshold includes 60, unlike its reader.
            let mut inode = fs
                .inner
                .create_inode(InodeCreationOptions {
                    file_type: ext4plus::FileType::Regular,
                    mode: InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o777),
                    uid: 0,
                    gid: 0,
                    time: Duration::ZERO,
                    flags: InodeFlags::empty(),
                })
                .await?;
            if write_at(&fs.inner, &mut inode, target.as_str().as_bytes(), 0).await? != 60 {
                return Err(KernelError::Other("short symlink write"));
            }
            inode.set_mode(InodeMode::S_IFLNK | InodeMode::from_bits_truncate(0o777))?;
            inode.write(&fs.inner).await?;
            inner_dir.link(entry, &mut inode).await?;
            return Ok(());
        }
        fs.inner
            .symlink(
                inner_dir,
                entry,
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

    async fn getxattr(&self, name: &str) -> Result<Vec<u8>> {
        let inner = self.inner.lock().await;
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;
        Ok(inner
            .get_xattr(&fs.inner, name)
            .await?
            .ok_or(FsError::NotFound)?)
    }

    async fn listxattr(&self) -> Result<Vec<String>> {
        let inner = self.inner.lock().await;
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;
        let mut xattrs = vec![];
        for attr in inner.list_xattrs(&fs.inner).await? {
            let str_attr = String::from_utf8_lossy(&attr).to_string();
            xattrs.push(str_attr);
        }
        Ok(xattrs)
    }

    async fn setxattr(&self, name: &str, buf: &[u8], create: bool, replace: bool) -> Result<()> {
        let mut inner = self.inner.lock().await;
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;
        if inner.get_xattr(&fs.inner, name).await?.is_some() {
            if create {
                return Err(KernelError::Fs(FsError::AlreadyExists));
            }
        } else {
            if replace {
                return Err(KernelError::Fs(FsError::NotFound));
            }
        }
        inner.set_xattr(&fs.inner, name, buf).await?;
        Ok(())
    }

    async fn removexattr(&self, name: &str) -> Result<()> {
        let mut inner = self.inner.lock().await;
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;
        inner.remove_xattr(&fs.inner, name).await?;
        Ok(())
    }
}

/// An EXT4 filesystem instance.
///
/// Lock order: namespace, directory inode(s), child inode, inode cache.
/// Cache guards are never held while awaiting an inode lock. Single-inode
/// data/attribute operations do not take namespace, keeping file I/O parallel.
pub struct Ext4Filesystem<CPU: CpuOps> {
    inner: Ext4,
    id: u64,
    this: Weak<Ext4Filesystem<CPU>>,
    namespace: Mutex<(), CPU>,
    inode_cache: Mutex<InodeCache<Mutex<InodeInner, CPU>>, CPU>,
    dev: Arc<Ext4Device<CPU>>,
    disk: DiskLayout,
    _phantom_data: PhantomData<CPU>,
}

impl<CPU> Ext4Filesystem<CPU>
where
    CPU: CpuOps + Send + Sync,
{
    // Caller holds namespace. On a cache miss, refresh the inode after checking
    // the cache: a writer could have closed the last handle since the parent's
    // get_entry read its snapshot. No new writer can open it during this reload.
    async fn shared_inode_inner(&self, inner: InodeInner) -> Result<Arc<Mutex<InodeInner, CPU>>> {
        let inode_id = inner.index;
        if let Some(existing) = self.inode_cache.lock().await.get(inode_id) {
            return Ok(existing);
        }
        let fresh = ExtInode::read(&self.inner, inode_id).await?;
        let inner = InodeInner::new(fresh, &self.inner).await;
        Ok(self
            .inode_cache
            .lock()
            .await
            .get_or_insert_with(inode_id, || Mutex::new(inner)))
    }

    // Caller holds namespace and parent. Keep the child locked from selecting
    // its authoritative inode through disk mutation and cache publication.
    async fn unlink_locked(&self, parent: &mut Dir, name: &str) -> Result<()> {
        let entry = Self::entry_name(name)?;
        let snapshot = parent.get_entry(entry).await?;
        if snapshot.index == parent.inode().index {
            return Err(FsError::InvalidInput.into());
        }
        let id = snapshot.index;
        let child = self
            .shared_inode_inner(InodeInner::new(snapshot, &self.inner).await)
            .await?;
        let mut child = child.lock().await;
        let is_dir = child.file_type() == ext4plus::FileType::Directory;
        if is_dir {
            let mut entries = Dir::open_inode(&self.inner, child.clone())?.read_dir()?;
            while let Some(entry) = entries.next().await {
                let entry = entry?;
                if !matches!(entry.file_name().as_str().unwrap(), "." | "..") {
                    return Err(FsError::DirectoryNotEmpty.into());
                }
            }
        }
        let mut to_unlink = child.clone();
        let mut saved = None;
        if is_dir {
            // rmdir removes both the parent name and the child's "." link.
            to_unlink.set_links_count(1);
        } else if child.file_type() == ext4plus::FileType::Symlink
            && child.links_count() == 1
            && child.fs_blocks(&self.inner)? == 0
        {
            child.write(&self.inner).await?;
            let original = self.disk.clear_inline_target(&self.dev, id).await?;
            to_unlink = match ExtInode::read(&self.inner, id).await {
                Ok(inode) => inode,
                Err(error) => {
                    original.restore(&self.dev).await?;
                    return Err(error.into());
                }
            };
            saved = Some(original);
        }
        let remaining = match parent.unlink(entry, to_unlink).await {
            Ok(remaining) => remaining,
            Err(error) => {
                // If removal failed before deleting the name, keep its target
                // intact. Never restore an inode after the name was removed:
                // allocation/freeing may already have progressed.
                if let Some(original) = saved {
                    if parent
                        .get_entry(entry)
                        .await
                        .is_ok_and(|inode| inode.index == id)
                    {
                        original.restore(&self.dev).await?;
                    }
                }
                return Err(error.into());
            }
        };
        if is_dir {
            let links = parent.inode().links_count().saturating_sub(1);
            parent.inode_mut().set_links_count(links);
            parent.inode_mut().write(&self.inner).await?;
        }
        if let Some(inode) = remaining {
            *child = InodeInner::new(inode, &self.inner).await;
        } else {
            child.set_links_count(0);
            // Evict before releasing namespace: a reused inode number must not
            // be associated with an older, still-open wrapper.
            self.inode_cache.lock().await.remove(id);
        }
        Ok(())
    }

    fn entry_name(name: &str) -> Result<DirEntryName<'_>> {
        if name == "." || name == ".." {
            return Err(FsError::InvalidInput.into());
        }
        DirEntryName::try_from(name).map_err(|_| FsError::InvalidInput.into())
    }

    // None means source and destination are the very same directory.
    async fn rename_locked(
        &self,
        mut old_dir: Option<&mut Dir>,
        new_dir: &mut Dir,
        old_name: &str,
        new_name: &str,
        no_replace: bool,
    ) -> Result<()> {
        let old_entry = Self::entry_name(old_name)?;
        let new_entry = Self::entry_name(new_name)?;
        let source = old_dir
            .as_deref()
            .unwrap_or(new_dir)
            .get_entry(old_entry)
            .await?;
        let target = match new_dir.get_entry(new_entry).await {
            Ok(inode) => Some(inode),
            Err(Ext4Error::NotFound) => None,
            Err(e) => return Err(e.into()),
        };
        if no_replace && target.is_some() {
            return Err(FsError::AlreadyExists.into());
        }
        if target
            .as_ref()
            .is_some_and(|inode| inode.index == source.index)
        {
            return Ok(());
        }

        // Reject moving a directory into itself or a descendant. Walk inode
        // numbers rather than wrapper paths, which can be stale after rename.
        if source.file_type() == ext4plus::FileType::Directory {
            let mut ancestor = new_dir.inode().clone();
            loop {
                if ancestor.index == source.index {
                    return Err(FsError::InvalidInput.into());
                }
                let index = ancestor.index;
                ancestor = Dir::open_inode(&self.inner, ancestor)?
                    .get_entry(DirEntryName::try_from("..").unwrap())
                    .await?;
                if ancestor.index == index {
                    break;
                }
            }
        }
        if let Some(target) = target {
            let source_is_dir = source.file_type() == ext4plus::FileType::Directory;
            let target_is_dir = target.file_type() == ext4plus::FileType::Directory;
            if source_is_dir != target_is_dir {
                return Err(if target_is_dir {
                    FsError::IsADirectory
                } else {
                    FsError::NotADirectory
                }
                .into());
            }
            if target_is_dir {
                let mut entries = Dir::open_inode(&self.inner, target)?.read_dir()?;
                while let Some(entry) = entries.next().await {
                    let entry = entry?;
                    if !matches!(entry.file_name().as_str().unwrap(), "." | "..") {
                        return Err(FsError::DirectoryNotEmpty.into());
                    }
                }
            }
            self.unlink_locked(new_dir, new_name).await?;
        }
        let child = self
            .shared_inode_inner(InodeInner::new(source, &self.inner).await)
            .await?;
        let mut child = child.lock().await;
        let is_dir = child.file_type() == ext4plus::FileType::Directory;
        let child_links = child.links_count();
        let parent_links = new_dir.inode().links_count();
        // Link first so unlink cannot free the source inode. Both parents and
        // the shared child stay locked until the final metadata is published.
        if let Err(error) = new_dir.link(new_entry, &mut child).await {
            // Dir::link updates link counts before inserting the directory
            // entry. Undo those counts if insertion failed.
            child.set_links_count(child_links);
            child.write(&self.inner).await?;
            if is_dir {
                new_dir.inode_mut().set_links_count(parent_links);
                new_dir.inode_mut().write(&self.inner).await?;
            }
            return Err(error.into());
        }
        let source_dir = old_dir.as_deref_mut().unwrap_or(new_dir);
        let remaining = match source_dir.unlink(old_entry, child.clone()).await {
            Ok(remaining) => remaining.ok_or(FsError::InvalidFs)?,
            Err(error) => {
                let inode = new_dir
                    .unlink(new_entry, child.clone())
                    .await?
                    .ok_or(FsError::InvalidFs)?;
                *child = InodeInner::new(inode, &self.inner).await;
                if is_dir {
                    new_dir.inode_mut().set_links_count(parent_links);
                    new_dir.inode_mut().write(&self.inner).await?;
                }
                return Err(error.into());
            }
        };
        *child = InodeInner::new(remaining, &self.inner).await;
        if is_dir {
            let old_parent_id = source_dir.inode().index;
            let old_links = source_dir.inode().links_count().saturating_sub(1);
            source_dir.inode_mut().set_links_count(old_links);
            source_dir.inode_mut().write(&self.inner).await?;
            // The same-parent case just undoes Dir::link's parent increment.
            if old_dir.is_some() {
                self.disk
                    .reparent(
                        &self.dev,
                        &self.inner,
                        &mut child,
                        old_parent_id,
                        new_dir.inode().index,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Construct a new EXT4 filesystem instance.
    pub async fn new(dev: BlockBuffer, id: u64) -> Result<Arc<Self>> {
        let dev_arc = Arc::new(Ext4Device {
            buffer: dev,
            writes: Mutex::new(()),
        });
        let inner =
            Ext4::load_with_writer(Box::new(dev_arc.clone()), Some(Box::new(dev_arc.clone())))
                .await?;
        let disk = DiskLayout::load(&dev_arc).await?;
        Ok(Arc::new_cyclic(|weak| Self {
            inner,
            id,
            this: weak.clone(),
            namespace: Mutex::new(()),
            inode_cache: Mutex::new(InodeCache::new()),
            dev: dev_arc,
            disk,
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
        let _namespace = self.namespace.lock().await;
        let root = self.inner.read_root_inode().await?;
        let root_id = root.index;
        let root_inner = self
            .shared_inode_inner(InodeInner::new(root, &self.inner).await)
            .await?;
        Ok(Arc::new(Ext4Inode::<CPU> {
            fs_ref: self.this.clone(),
            id: root_id,
            inner: root_inner,
            path: ExtPathBuf::new("/"),
        }))
    }

    /// Flushes any dirty data to the underlying block device.  The current
    /// stub implementation simply forwards the request to `BlockBuffer::sync`.
    async fn sync(&self) -> Result<()> {
        self.dev.buffer.sync().await?;
        Ok(())
    }
}
