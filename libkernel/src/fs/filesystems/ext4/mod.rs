//! EXT4 Filesystem Driver

#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

use crate::error::FsError;
use crate::fs::pathbuf::PathBuf;
use crate::fs::{DirStream, Dirent};
use crate::proc::ids::{Gid, Uid};
use crate::{
    error::{KernelError, Result},
    fs::{
        FileType, Filesystem, Inode, InodeId,
        attr::{FileAttr, FilePermissions},
        blk::buffer::BlockBuffer,
    },
};
use alloc::string::ToString;
use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use async_trait::async_trait;
use core::error::Error;
use ext4_view::{AsyncIterator, AsyncSkip, Ext4, Ext4Read, FollowSymlinks, Metadata, ReadDir};

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

impl From<ext4_view::Ext4Error> for KernelError {
    fn from(err: ext4_view::Ext4Error) -> Self {
        match err {
            ext4_view::Ext4Error::NotFound => KernelError::Fs(FsError::NotFound),
            ext4_view::Ext4Error::NotADirectory => KernelError::Fs(FsError::NotADirectory),
            _ => KernelError::Fs(FsError::InvalidFs),
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
            mode: FilePermissions::from_bits(meta.mode.bits()).unwrap(),
            uid: Uid::new(meta.uid),
            gid: Gid::new(meta.gid),
            ..Default::default()
        }
    }
}

pub struct ReadDirWrapper {
    inner: AsyncSkip<ReadDir>,
    fs_id: u64,
}

impl ReadDirWrapper {
    pub fn new(inner: ReadDir, fs_id: u64, start_offset: u64) -> Self {
        Self {
            inner: inner.skip(start_offset as usize),
            fs_id,
        }
    }
}

#[async_trait]
impl DirStream for ReadDirWrapper {
    async fn next_entry(&mut self) -> Result<Option<Dirent>> {
        match self.inner.next().await {
            Some(entry) => {
                let entry = entry?;
                Ok(Some(Dirent {
                    id: InodeId::from_fsid_and_inodeid(self.fs_id, entry.inode.get() as u64),
                    name: entry.file_name().as_str().unwrap().to_string(),
                    file_type: entry.file_type()?.into(),
                    offset: 0,
                }))
            }
            None => Ok(None),
        }
    }
}

pub struct Ext4Inode {
    fs_ref: Weak<Ext4Filesystem>,
    inner: ext4_view::Inode,
    path: ext4_view::PathBuf,
}

#[async_trait]
impl Inode for Ext4Inode {
    fn id(&self) -> InodeId {
        let fs = self.fs_ref.upgrade().unwrap();
        InodeId::from_fsid_and_inodeid(fs.id(), self.inner.index.get() as u64)
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if self.inner.metadata.file_type != ext4_view::FileType::Regular {
            return Err(KernelError::NotSupported);
        }
        let fs = self.fs_ref.upgrade().unwrap();
        // TODO: Optimize to read directly into buf instead of allocating a new Vec
        let vec = fs.inner.read_inode_file(&self.inner).await?;
        let file_size = vec.len() as u64;
        if offset >= file_size {
            return Ok(0);
        }
        let to_read = core::cmp::min(buf.len() as u64, file_size - offset) as usize;
        buf[..to_read].copy_from_slice(&vec[offset as usize..(offset as usize + to_read)]);
        Ok(to_read)
    }

    async fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize> {
        Err(KernelError::NotSupported)
    }

    async fn truncate(&self, _size: u64) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.inner.metadata.clone().into())
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        let fs = self.fs_ref.upgrade().unwrap();
        let child_path = self.path.join(name);
        let child_inode = fs
            .inner
            .path_to_inode(child_path.as_path(), FollowSymlinks::All)
            .await?;
        Ok(Arc::new(Ext4Inode {
            fs_ref: self.fs_ref.clone(),
            inner: child_inode,
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
        if self.inner.metadata.file_type != ext4_view::FileType::Directory {
            return Err(KernelError::NotSupported);
        }
        let fs = self.fs_ref.upgrade().unwrap();
        Ok(Box::new(ReadDirWrapper::new(
            ReadDir::new(fs.inner.clone(), &self.inner, self.path.clone())?,
            fs.id(),
            start_offset,
        )))
    }
}

/// An EXT4 filesystem instance.
///
/// For now this struct only stores the underlying block buffer and an ID
/// assigned by the VFS when the filesystem is mounted.
pub struct Ext4Filesystem {
    inner: Ext4,
    id: u64,
    this: Weak<Ext4Filesystem>,
}

impl Ext4Filesystem {
    /// Construct a new EXT4 filesystem instance.
    pub async fn new(dev: BlockBuffer, id: u64) -> Result<Arc<Self>> {
        let inner = Ext4::load(Box::new(dev)).await?;
        Ok(Arc::new_cyclic(|weak| Self {
            inner,
            id,
            this: weak.clone(),
        }))
    }
}

#[async_trait]
impl Filesystem for Ext4Filesystem {
    fn id(&self) -> u64 {
        self.id
    }

    /// Returns the root inode of the mounted EXT4 filesystem.
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Ok(Arc::new(Ext4Inode {
            fs_ref: self.this.clone(),
            inner: self.inner.read_root_inode().await?,
            path: ext4_view::PathBuf::new("/"),
        }))
    }

    /// Flushes any dirty data to the underlying block device.  The current
    /// stub implementation simply forwards the request to `BlockBuffer::sync`.
    async fn sync(&self) -> Result<()> {
        Ok(())
    }
}
