//! EXT4 Filesystem Driver

#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

use crate::error::FsError;
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
use core::error::Error;
use core::marker::PhantomData;
use core::num::NonZeroU32;
use ext4_view::{
    AsyncIterator, AsyncSkip, Ext4, Ext4Read, Ext4Write, File, FollowSymlinks, Metadata, ReadDir,
    get_dir_entry_inode_by_name,
};
use log::{error, info};

#[async_trait]
impl Ext4Read for BlockBuffer {
    async fn read(
        &self,
        start_byte: u64,
        dst: &mut [u8],
    ) -> core::result::Result<(), Box<dyn Error + Send + Sync + 'static>> {
        info!("EXT4 read: offset={}, len={}", start_byte, dst.len());
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

impl From<ext4_view::Ext4Error> for KernelError {
    fn from(err: ext4_view::Ext4Error) -> Self {
        match err {
            ext4_view::Ext4Error::NotFound => KernelError::Fs(FsError::NotFound),
            ext4_view::Ext4Error::NotADirectory => KernelError::Fs(FsError::NotADirectory),
            ext4_view::Ext4Error::Corrupt(_) => KernelError::Fs(FsError::InvalidFs),
            e => {
                error!("Unmapped EXT4 error: {:?}", e);
                KernelError::Other("EXT4 error")
            }
        }
    }
}

impl From<ext4_view::FileType> for FileType {
    fn from(ft: ext4_view::FileType) -> Self {
        match ft {
            ext4_view::FileType::BlockDevice => todo!(),
            ext4_view::FileType::CharacterDevice => todo!(),
            ext4_view::FileType::Directory => FileType::Directory,
            ext4_view::FileType::Fifo => FileType::Fifo,
            ext4_view::FileType::Regular => FileType::File,
            ext4_view::FileType::Socket => FileType::Socket,
            ext4_view::FileType::Symlink => FileType::Symlink,
        }
    }
}

impl From<Metadata> for FileAttr {
    fn from(meta: Metadata) -> Self {
        FileAttr {
            size: meta.size_in_bytes,
            file_type: meta.file_type.into(),
            // Infallible, since they are identical
            mode: FilePermissions::from_bits(meta.mode.bits()).unwrap(),
            uid: Uid::new(meta.uid),
            gid: Gid::new(meta.gid),
            atime: meta.atime,
            ctime: meta.ctime,
            mtime: meta.mtime,
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

pub struct Ext4Inode<CPU: CpuOps> {
    fs_ref: Weak<Ext4Filesystem<CPU>>,
    id: NonZeroU32,
    inner: Mutex<ext4_view::Inode, CPU>,
    path: ext4_view::PathBuf,
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
        if inner.metadata.file_type != ext4_view::FileType::Regular {
            return Err(KernelError::NotSupported);
        }

        let file_size = inner.metadata.size_in_bytes;

        // Past EOF = nothing to read.
        if offset >= file_size {
            return Ok(0);
        }

        // Do not read past the end of the file.
        let to_read = core::cmp::min(buf.len() as u64, file_size - offset) as usize;

        let fs = self.fs_ref.upgrade().unwrap();
        let mut file = File::open_inode(&fs.inner, inner.clone())?;

        file.seek_to(offset).await?;

        // `ext4_view::File::read_bytes` may return fewer bytes than requested
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
        if inner.metadata.file_type != ext4_view::FileType::Regular {
            return Err(KernelError::NotSupported);
        }

        let fs = self.fs_ref.upgrade().unwrap();
        let mut file = File::open_inode(&fs.inner, inner.clone())?;

        file.seek_to(offset).await?;

        // `ext4_view::File::write_bytes` may write fewer bytes than requested
        // if the write crosses a block boundary. Loop until we've written
        // all bytes.
        let mut total_written = 0;
        while total_written < buf.len() {
            let bytes_written = file.write_bytes(&buf[total_written..]).await?;
            if bytes_written == 0 {
                break; // Should not happen unless disk is full
            }
            total_written += bytes_written;
        }

        // Update inode metadata in case size changed.
        *inner = file.into_inode();

        Ok(total_written)
    }

    async fn truncate(&self, _size: u64) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn getattr(&self) -> Result<FileAttr> {
        let inner = self.inner.lock().await;
        let mut attrs: FileAttr = inner.metadata.clone().into();
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;

        attrs.id = InodeId::from_fsid_and_inodeid(fs.id(), self.id.get() as u64);

        Ok(attrs)
    }

    async fn setattr(&self, attr: FileAttr) -> Result<()> {
        let mut inner = self.inner.lock().await;
        inner.metadata.atime = attr.atime;
        inner.metadata.ctime = attr.ctime;
        inner.metadata.mtime = attr.mtime;
        inner.metadata.gid = attr.gid.into();
        inner.metadata.uid = attr.uid.into();
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;
        inner.write(&fs.inner).await?;
        Ok(())
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        let fs = self.fs_ref.upgrade().unwrap();
        let inner = self.inner.lock().await;
        let child_inode = get_dir_entry_inode_by_name(
            &fs.inner,
            &inner,
            ext4_view::DirEntryName::try_from(name)
                .map_err(|_| KernelError::Fs(FsError::InvalidInput))?,
        )
        .await?;
        let child_path = self.path.join(name);
        Ok(Arc::new(Ext4Inode::<CPU> {
            fs_ref: self.fs_ref.clone(),
            id: child_inode.index,
            inner: Mutex::new(child_inode),
            path: child_path,
        }))
    }

    async fn create(
        &self,
        _name: &str,
        _file_type: FileType,
        _permissions: FilePermissions,
    ) -> Result<Arc<dyn Inode>> {
        Err(KernelError::NotSupported)
    }

    async fn unlink(&self, _name: &str) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let inner = self.inner.lock().await;
        if inner.metadata.file_type != ext4_view::FileType::Directory {
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
        if inner.metadata.file_type != ext4_view::FileType::Symlink {
            return Err(KernelError::NotSupported);
        }
        let fs = self.fs_ref.upgrade().unwrap();
        // Conversion has to ensure path is valid UTF-8 (O(n) time).
        Ok(inner
            .symlink_target(&fs.inner)
            .await
            .map(|p| PathBuf::from(p.to_str().unwrap()))?)
    }

    async fn sync(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        let fs = self.fs_ref.upgrade().ok_or(FsError::InvalidFs)?;
        inner.write(&fs.inner).await?;
        Ok(())
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
        info!("Loading EXT4 filesystem...");
        let inner =
            Ext4::load_with_writer(Box::new(dev_arc.clone()), Some(Box::new(dev_arc.clone())))
                .await?;
        info!("EXT4 filesystem loaded successfully.");
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

    /// Returns the root inode of the mounted EXT4 filesystem.
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        let root = self.inner.read_root_inode().await?;
        Ok(Arc::new(Ext4Inode::<CPU> {
            fs_ref: self.this.clone(),
            id: root.index,
            inner: Mutex::new(root),
            path: ext4_view::PathBuf::new("/"),
        }))
    }

    /// Flushes any dirty data to the underlying block device.  The current
    /// stub implementation simply forwards the request to `BlockBuffer::sync`.
    async fn sync(&self) -> Result<()> {
        self.dev.sync().await?;
        Ok(())
    }
}
