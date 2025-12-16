//! EXT4 Filesystem Driver

#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

mod group;
mod inode;
mod superblock;
mod dir_entry;

use group::Ext4BlockGroupDescriptor;
use inode::Ext4Inode;
use superblock::Ext4SuperBlock;

use crate::proc::ids::{Gid, Uid};
use crate::{
    error::{KernelError, Result},
    fs::{
        FileType, Filesystem, Inode, InodeId,
        attr::{FileAttr, FilePermissions},
        blk::buffer::BlockBuffer,
    },
};
use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use async_trait::async_trait;

pub const EXT4_NAME_LEN: usize = 255;

/// An EXT4 filesystem instance.
///
/// For now this struct only stores the underlying block buffer and an ID
/// assigned by the VFS when the filesystem is mounted.
pub struct Ext4Filesystem {
    dev: BlockBuffer,
    superblock: Ext4SuperBlock,
    id: u64,
    this: Weak<Self>,
}

impl Ext4Filesystem {
    /// Construct a new EXT4 filesystem instance.
    pub async fn new(dev: BlockBuffer, id: u64) -> Result<Arc<Self>> {
        // The EXT super-block lives at byte offset 1024.  Read it as a POD
        // structure and do a very small sanity check so we fail fast on
        // non-EXT images.
        let sb: Ext4SuperBlock = dev.read_obj(1024).await?;
        if sb.magic != 0xEF53 {
            return Err(KernelError::Fs(crate::error::FsError::InvalidFs));
        }

        Ok(Arc::new_cyclic(|weak| Self {
            dev,
            superblock: sb,
            id,
            this: weak.clone(),
        }))
    }

    /// Reads a block-group descriptor from the primary descriptor table.
    ///
    /// The EXT4 primary GDT starts immediately after the super-block:
    ///   • For 1 KiB block-size the SB is at block 1 ⇒ GDT begins at block 2
    ///   • For larger block-sizes the SB is at block 0 ⇒ GDT begins at block 1
    ///
    /// Each descriptor is `desc_size` bytes (default 32).  Both `desc_size`
    /// and `log_block_size` come from the super-block, so no additional I/O is
    /// needed to compute the exact byte offset.
    pub async fn read_group_desc(&self, group_idx: u32) -> Result<Ext4BlockGroupDescriptor> {
        let block_size = 1024u64 << self.superblock.log_block_size;

        let desc_size = if self.superblock.desc_size == 0 {
            32usize
        } else {
            self.superblock.desc_size as usize
        };

        let descriptor_block = if block_size == 1024 { 2u64 } else { 1u64 };

        let offset = descriptor_block * block_size + group_idx as u64 * desc_size as u64;

        self.dev.read_obj(offset).await
    }

    /// Reads an inode from disk and returns the on-disk structure.
    pub async fn read_inode(&self, ino: u32) -> Result<Ext4Inode> {
        if ino == 0 {
            return Err(KernelError::InvalidValue);
        }

        let inodes_per_group = self.superblock.inodes_per_group;
        let group_idx = (ino - 1) / inodes_per_group;
        let index = (ino - 1) % inodes_per_group;

        let desc = self.read_group_desc(group_idx).await?;

        let table_block = (desc.inode_table_lo as u64) | ((desc.inode_table_hi as u64) << 32);

        let block_size = 1024u64 << self.superblock.log_block_size;
        let inode_size = if self.superblock.inode_size == 0 {
            128u64
        } else {
            self.superblock.inode_size as u64
        };

        let offset = table_block * block_size + index as u64 * inode_size;

        self.dev.read_obj(offset).await
    }
}

/// A placeholder directory inode that represents `"/"` on an EXT4 volume.
///
/// Every method returns `KernelError::NotSupported`; it only exists so that
/// `Ext4Filesystem::root_inode()` has something concrete to hand back.
struct Ext4RootInode {
    fs: Weak<Ext4Filesystem>,
    attr: FileAttr,
}

impl Ext4RootInode {
    fn new(fs: Weak<Ext4Filesystem>, attr: FileAttr) -> Self {
        Self { fs, attr }
    }
}

#[async_trait]
impl Inode for Ext4RootInode {
    fn id(&self) -> InodeId {
        self.attr.id
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize> {
        Err(KernelError::NotSupported)
    }

    async fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize> {
        Err(KernelError::NotSupported)
    }

    async fn truncate(&self, _size: u64) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn lookup(&self, _name: &str) -> Result<Arc<dyn Inode>> {
        Err(KernelError::NotSupported)
    }

    async fn create(
        &self,
        _name: &str,
        _file_type: FileType,
        _permissions: u16,
    ) -> Result<Arc<dyn Inode>> {
        Err(KernelError::NotSupported)
    }

    async fn unlink(&self, _name: &str) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn readdir(&self, _start_offset: u64) -> Result<Box<dyn crate::fs::DirStream>> {
        Err(KernelError::NotSupported)
    }
}

#[async_trait]
impl Filesystem for Ext4Filesystem {
    fn id(&self) -> u64 {
        self.id
    }

    /// Returns the root inode of the mounted EXT4 filesystem.
    ///
    /// At present this is a dummy inode that only supports `getattr`.
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        let dinode = self.read_inode(2).await?;
        let size = ((dinode.size_high as u64) << 32) | dinode.size_lo as u64;

        let mode_bits = dinode.mode;
        let file_type = match mode_bits & 0xF000 {
            0x4000 => FileType::Directory,
            0x8000 => FileType::File,
            0xA000 => FileType::Symlink,
            _ => FileType::File,
        };
        let permissions = FilePermissions::from_bits_truncate((mode_bits & 0o777) as u16);
        let uid = Uid::new(dinode.uid as u32);
        let gid = Gid::new(dinode.gid as u32);

        let attr = FileAttr {
            id: InodeId::from_fsid_and_inodeid(self.id, 2),
            file_type,
            mode: permissions,
            uid,
            gid,
            size,
            ..FileAttr::default()
        };

        Ok(Arc::new(Ext4RootInode::new(self.this.clone(), attr)))
    }

    /// Flushes any dirty data to the underlying block device.  The current
    /// stub implementation simply forwards the request to `BlockBuffer::sync`.
    async fn sync(&self) -> Result<()> {
        self.dev.sync().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::BlockDevice;
    use async_trait::async_trait;

    /// Simple in-memory block device backed by the embedded ext4 test image.
    struct MemBlkDevice {
        data: &'static [u8],
    }

    #[async_trait]
    impl BlockDevice for MemBlkDevice {
        async fn read(&self, block_id: u64, buf: &mut [u8]) -> Result<()> {
            // EXT4 sector size is typically 512. We use that here.
            const BLOCK_SIZE: usize = 512;
            let offset = (block_id as usize) * BLOCK_SIZE;
            let end = offset + buf.len();
            buf.copy_from_slice(&self.data[offset..end]);
            Ok(())
        }

        async fn write(&self, _block_id: u64, _buf: &[u8]) -> Result<()> {
            Err(KernelError::NotSupported)
        }

        fn block_size(&self) -> usize {
            512
        }

        async fn sync(&self) -> Result<()> {
            Ok(())
        }
    }

    const IMG: &[u8] = include_bytes!("test_img/test.img");

    #[tokio::test]
    async fn test_mount_ext4_image() {
        // Wrap in our memory block device → BlockBuffer.
        let blk_buf = BlockBuffer::new(Box::new(MemBlkDevice { data: IMG }));

        // Attempt to mount.
        let fs = Ext4Filesystem::new(blk_buf, 42)
            .await
            .expect("Failed to mount ext4 image");

        // Fetch and inspect the root inode.
        let root = fs.root_inode().await.expect("root inode");
        let attr = root.getattr().await.expect("getattr");

        assert_eq!(attr.file_type, FileType::Directory);
        assert_eq!(attr.id.fs_id(), 42);
    }

    #[tokio::test]
    async fn test_read_group_desc() {
        let blk_buf = BlockBuffer::new(Box::new(MemBlkDevice { data: IMG }));
        let fs = Ext4Filesystem::new(blk_buf, 1).await.expect("mount");
        let desc = fs.read_group_desc(0).await.expect("group desc");
        // The inode table block for group 0 must be non-zero in a valid image.
        assert!(desc.inode_table_lo != 0 || desc.inode_table_hi != 0);
    }
}
