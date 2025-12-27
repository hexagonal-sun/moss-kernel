//! Virtual Filesystem (VFS) Interface Definitions
//!
//! This module defines the core traits and data structures for the kernel's I/O subsystem.
//! It is based on a layered design:
//!
//! 1. `BlockDevice`: An abstraction for raw block-based hardware (e.g., disks).
//! 2. `Filesystem`: An abstraction for a mounted filesystem instance (e.g.,
//!    ext4, fat32). Its main role is to provide the root `Inode`.
//! 3. `Inode`: A stateless representation of a filesystem object (file,
//!    directory, etc.). It handles operations by explicit offsets (`read_at`,
//!    `write_at`).
//! 4. `File`: A stateful open file handle. It maintains a cursor and provides
//!    the familiar `read`, `write`, and `seek` operations.
extern crate alloc;

pub mod attr;
pub mod blk;
pub mod filesystems;
pub mod path;
pub mod pathbuf;

use core::any::Any;

use crate::{
    driver::CharDevDescriptor,
    error::{FsError, KernelError, Result},
    fs::{path::Path, pathbuf::PathBuf},
};
use alloc::{boxed::Box, string::String, sync::Arc};
use async_trait::async_trait;
use attr::{FileAttr, FilePermissions};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct OpenFlags: u32 {
        const O_RDONLY    = 0b000;
        const O_WRONLY    = 0b001;
        const O_RDWR      = 0b010;
        const O_ACCMODE   = 0b011;
        const O_CREAT     = 0o100;
        const O_EXCL      = 0o200;
        const O_TRUNC     = 0o1000;
        const O_DIRECTORY = 0o200000;
        const O_APPEND    = 0o2000;
        const O_NONBLOCK  = 0o4000;
        const O_CLOEXEC   = 0o2000000;
    }
}

// Reserved psuedo filesystem instances created internally in the kernel.
pub const DEVFS_ID: u64 = 1;
pub const PROCFS_ID: u64 = 2;
pub const FS_ID_START: u64 = 10;

/// Trait for a mounted filesystem instance. Its main role is to act as a
/// factory for Inodes.
#[async_trait]
pub trait Filesystem: Send + Sync {
    /// Get the root inode of this filesystem.
    async fn root_inode(&self) -> Result<Arc<dyn Inode>>;

    /// Returns the instance ID for this FS.
    fn id(&self) -> u64;

    /// Flushes all pending data to the underlying storage device(s).
    ///
    /// The default implementation is a no-op so that read-only filesystems do
    /// not need to override it.
    async fn sync(&self) -> Result<()> {
        Ok(())
    }
}

// A unique identifier for an inode across the entire VFS. A tuple of
// (filesystem_id, inode_number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct InodeId(u64, u64);

impl InodeId {
    pub fn from_fsid_and_inodeid(fs_id: u64, inode_id: u64) -> Self {
        Self(fs_id, inode_id)
    }

    pub fn dummy() -> Self {
        Self(u64::MAX, u64::MAX)
    }

    pub fn fs_id(self) -> u64 {
        self.0
    }

    pub fn inode_id(self) -> u64 {
        self.1
    }
}

/// Standard POSIX file types.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    Symlink,
    BlockDevice(CharDevDescriptor),
    CharDevice(CharDevDescriptor),
    Fifo,
    Socket,
}

impl From<FileType> for u32 {
    fn from(file_type: FileType) -> Self {
        match file_type {
            FileType::Directory => 0o040000,
            FileType::CharDevice(_) => 0o020000,
            FileType::BlockDevice(_) => 0o060000,
            FileType::File => 0o100000,
            FileType::Fifo => 0o010000,
            FileType::Symlink => 0o120000,
            FileType::Socket => 0o140000,
        }
    }
}

/// A stateful, streaming iterator for reading directory entries.
#[async_trait]
pub trait DirStream: Send + Sync {
    /// Fetches the next directory entry in the stream. Returns `Ok(None)` when
    /// the end of the directory is reached.
    async fn next_entry(&mut self) -> Result<Option<Dirent>>;
}

/// Represents a single directory entry.
#[derive(Debug, Clone)]
pub struct Dirent {
    pub id: InodeId,
    pub name: String,
    pub file_type: FileType,
    pub offset: u64,
}

impl Dirent {
    pub fn new(name: String, id: InodeId, file_type: FileType, offset: u64) -> Self {
        Self {
            id,
            name,
            file_type,
            offset,
        }
    }
}

/// Specifies how to seek within a file, mirroring `std::io::SeekFrom`.
#[derive(Debug, Copy, Clone)]
pub enum SeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

/// Trait for a raw block device.
#[async_trait]
pub trait BlockDevice: Send + Sync {
    /// Read one or more blocks starting at `block_id`.
    /// The `buf` length must be a multiple of `block_size`.
    async fn read(&self, block_id: u64, buf: &mut [u8]) -> Result<()>;

    /// Write one or more blocks starting at `block_id`.
    /// The `buf` length must be a multiple of `block_size`.
    async fn write(&self, block_id: u64, buf: &[u8]) -> Result<()>;

    /// The size of a single block in bytes.
    fn block_size(&self) -> usize;

    /// Flushes any caches to the underlying device.
    async fn sync(&self) -> Result<()>;
}

/// A stateless representation of a filesystem object.
///
/// This trait represents an object on the disk (a file, a directory, etc.). All
/// operations are stateless from the VFS's perspective; for instance, `read_at`
/// takes an explicit offset instead of using a hidden cursor.
#[async_trait]
pub trait Inode: Send + Sync + Any {
    /// Get the unique ID for this inode.
    fn id(&self) -> InodeId;

    /// Reads data from the inode at a specific `offset`.
    /// Returns the number of bytes read.
    async fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize> {
        Err(KernelError::NotSupported)
    }

    /// Writes data to the inode at a specific `offset`.
    /// Returns the number of bytes written.
    async fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize> {
        Err(KernelError::NotSupported)
    }

    /// Truncates the inode to a specific `size`.
    async fn truncate(&self, _size: u64) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    /// Gets the metadata for this inode.
    async fn getattr(&self) -> Result<FileAttr> {
        Err(KernelError::NotSupported)
    }

    /// Sets the metadata for this inode.
    async fn setattr(&self, _attr: FileAttr) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    /// Looks up a name within a directory, returning the corresponding inode.
    async fn lookup(&self, _name: &str) -> Result<Arc<dyn Inode>> {
        Err(KernelError::NotSupported)
    }

    /// Creates a new object within a directory.
    async fn create(
        &self,
        _name: &str,
        _file_type: FileType,
        _permissions: FilePermissions,
    ) -> Result<Arc<dyn Inode>> {
        Err(KernelError::NotSupported)
    }

    /// Removes a link to an inode from a directory.
    async fn unlink(&self, _name: &str) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    /// Creates a new link to an inode in a directory.
    async fn link(&self, _name: &str, _inode: Arc<dyn Inode>) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    /// Creates a new symlink
    async fn symlink(&self, _name: &str, _target: &Path) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn rename_from(
        &self,
        _old_parent: Arc<dyn Inode>,
        _old_name: &str,
        _new_name: &str,
        _no_replace: bool,
    ) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn exchange(
        &self,
        _first_name: &str,
        _second_parent: Arc<dyn Inode>,
        _second_name: &str,
    ) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    /// Checks if a directory is empty.
    fn dir_is_empty(&self) -> Result<bool> {
        Err(FsError::NotADirectory.into())
    }

    /// Reads the contents of a directory.
    async fn readdir(&self, _start_offset: u64) -> Result<Box<dyn DirStream>> {
        Err(FsError::NotADirectory.into())
    }

    /// Reads the path of a symlink.
    async fn readlink(&self) -> Result<PathBuf> {
        Err(KernelError::NotSupported)
    }
}
