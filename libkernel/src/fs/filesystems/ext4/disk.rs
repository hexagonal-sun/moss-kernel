//! The small set of on-disk edits not exposed by ext4plus's public API.
//!
//! Allocation, inode freeing, block mapping and directory indexing remain in
//! ext4plus. All offsets below describe the ext4 disk format, never Rust object
//! layout. See https://docs.kernel.org/filesystems/ext4/ondisk/index.html.

use super::*;
use ext4plus::prelude::read_at;

pub(super) struct DiskLayout {
    pub block_size: u64,
    inode_size: usize,
    inodes_per_group: u32,
    inodes_count: u32,
    descriptor_size: usize,
    descriptor_start: u64,
    blocks_count: u64,
    checksum_seed: Option<u32>,
}

pub(super) struct SavedInode {
    offset: u64,
    bytes: Vec<u8>,
}

impl SavedInode {
    pub async fn restore<CPU: CpuOps>(&self, dev: &Ext4Device<CPU>) -> Result<()> {
        dev.write_at(self.offset, &self.bytes).await
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

// ext4 uses the reflected Castagnoli CRC without the final complement.
fn crc32c(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

impl DiskLayout {
    pub async fn load<CPU: CpuOps>(dev: &Ext4Device<CPU>) -> Result<Self> {
        let mut sb = [0; 1024];
        dev.read_at(1024, &mut sb).await?;
        let log_block_size = u32_at(&sb, 0x18);
        let incompatible = u32_at(&sb, 0x60);
        if u16_at(&sb, 0x38) != 0xef53 || log_block_size > 6 {
            return Err(FsError::InvalidFs.into());
        }
        // META_BG relocates descriptor tables. ext4plus does not support it
        // either; do not guess inode locations on such a filesystem.
        if incompatible & 0x10 != 0 {
            return Err(KernelError::NotSupported);
        }
        let block_size = 1024u64 << log_block_size;
        let is_64bit = incompatible & 0x80 != 0;
        let descriptor_size = if is_64bit {
            usize::from(u16_at(&sb, 0xfe))
        } else {
            32
        };
        let inode_size = if u32_at(&sb, 0x4c) == 0 {
            128
        } else {
            usize::from(u16_at(&sb, 0x58))
        };
        let inodes_per_group = u32_at(&sb, 0x28);
        if !matches!(descriptor_size, 32 | 64)
            || (is_64bit && descriptor_size != 64)
            || inode_size < 128
            || !inode_size.is_power_of_two()
            || inode_size as u64 > block_size
            || inodes_per_group == 0
        {
            return Err(FsError::InvalidFs.into());
        }
        let blocks_count = u64::from(u32_at(&sb, 4))
            | if is_64bit {
                u64::from(u32_at(&sb, 0x150)) << 32
            } else {
                0
            };
        let checksum_seed = (u32_at(&sb, 0x64) & 0x400 != 0).then(|| {
            if incompatible & 0x2000 != 0 {
                u32_at(&sb, 0x270)
            } else {
                crc32c(!0, &sb[0x68..0x78])
            }
        });
        Ok(Self {
            block_size,
            inode_size,
            inodes_per_group,
            inodes_count: u32_at(&sb, 0),
            descriptor_size,
            descriptor_start: (u64::from(u32_at(&sb, 0x14)) + 1) * block_size,
            blocks_count,
            checksum_seed,
        })
    }

    async fn read_inode<CPU: CpuOps>(
        &self,
        dev: &Ext4Device<CPU>,
        id: NonZeroU32,
    ) -> Result<(u64, Vec<u8>)> {
        if id.get() > self.inodes_count {
            return Err(FsError::InvalidFs.into());
        }
        let index = id.get() - 1;
        let group = index / self.inodes_per_group;
        let slot = index % self.inodes_per_group;
        let mut descriptor = vec![0; self.descriptor_size];
        dev.read_at(
            self.descriptor_start + u64::from(group) * self.descriptor_size as u64,
            &mut descriptor,
        )
        .await?;
        let table = u64::from(u32_at(&descriptor, 8))
            | if self.descriptor_size == 64 {
                u64::from(u32_at(&descriptor, 0x28)) << 32
            } else {
                0
            };
        let offset = table
            .checked_mul(self.block_size)
            .and_then(|start| start.checked_add(u64::from(slot) * self.inode_size as u64))
            .ok_or(FsError::InvalidFs)?;
        let end = offset
            .checked_add(self.inode_size as u64)
            .ok_or(FsError::InvalidFs)?;
        let disk_size = self
            .blocks_count
            .checked_mul(self.block_size)
            .ok_or(FsError::InvalidFs)?;
        if table == 0 || end > disk_size {
            return Err(FsError::InvalidFs.into());
        }
        let mut bytes = vec![0; self.inode_size];
        dev.read_at(offset, &mut bytes).await?;
        Ok((offset, bytes))
    }

    fn inode_seed(&self, id: NonZeroU32, bytes: &[u8]) -> Option<u32> {
        self.checksum_seed
            .map(|seed| crc32c(crc32c(seed, &id.get().to_le_bytes()), &bytes[0x64..0x68]))
    }

    fn checksum_inode(&self, id: NonZeroU32, bytes: &mut [u8]) {
        if let Some(seed) = self.inode_seed(id, bytes) {
            let high = bytes.len() >= 132 && u16_at(bytes, 0x80) >= 4;
            bytes[0x7c..0x7e].fill(0);
            if high {
                bytes[0x82..0x84].fill(0);
            }
            let crc = crc32c(seed, bytes).to_le_bytes();
            bytes[0x7c..0x7e].copy_from_slice(&crc[..2]);
            if high {
                bytes[0x82..0x84].copy_from_slice(&crc[2..]);
            }
        }
    }

    /// Clear the inline target before passing the last unlink to the library,
    /// whose generic block-map freeing otherwise treats target bytes as blocks.
    /// Caller holds the namespace and inode locks and has flushed the inode.
    pub async fn clear_inline_target<CPU: CpuOps>(
        &self,
        dev: &Ext4Device<CPU>,
        id: NonZeroU32,
    ) -> Result<SavedInode> {
        let (offset, mut bytes) = self.read_inode(dev, id).await?;
        if u16_at(&bytes, 0) & 0xf000 != 0xa000
            || u16_at(&bytes, 0x1a) != 1
            || u32_at(&bytes, 0x20) & 0x80000 != 0
            || !(1..60).contains(&u32_at(&bytes, 4))
            || u32_at(&bytes, 0x6c) != 0
            || u32_at(&bytes, 0x1c) != 0
            || u16_at(&bytes, 0x74) != 0
        {
            return Err(FsError::InvalidFs.into());
        }
        let saved = SavedInode {
            offset,
            bytes: bytes.clone(),
        };
        bytes[0x28..0x64].fill(0);
        self.checksum_inode(id, &mut bytes);
        if let Err(error) = dev.write_at(offset, &bytes).await {
            saved.restore(dev).await?;
            return Err(error);
        }
        Ok(saved)
    }

    /// Change only the existing ".." record, preserving a directory's htree.
    pub async fn reparent<CPU: CpuOps>(
        &self,
        dev: &Ext4Device<CPU>,
        fs: &Ext4,
        inode: &mut ExtInode,
        old: NonZeroU32,
        new: NonZeroU32,
    ) -> Result<()> {
        let mut block = vec![0; self.block_size as usize];
        if read_at(fs, inode, &mut block, 0).await? != block.len()
            || u32_at(&block, 0) != inode.index.get()
            || u16_at(&block, 4) != 12
            || block[6] != 1
            || block[8] != b'.'
            || u32_at(&block, 12) != old.get()
            || u16_at(&block, 16) < 12
            || usize::from(u16_at(&block, 16)) > block.len() - 12
            || block[18] != 2
            || &block[20..22] != b".."
        {
            return Err(FsError::InvalidFs.into());
        }
        let (_, bytes) = self.read_inode(dev, inode.index).await?;
        let seed = self.inode_seed(inode.index, &bytes);
        let checksum = |block: &[u8]| -> Result<Option<u32>> {
            let Some(seed) = seed else { return Ok(None) };
            let len = block.len();
            if inode.flags().contains(InodeFlags::DIRECTORY_HTREE) {
                let count = usize::from(u16_at(block, 0x22));
                let limit = usize::from(u16_at(block, 0x20));
                if block[0x1d] != 8 || count == 0 || count > limit || 32 + limit * 8 != len - 8 {
                    return Err(FsError::InvalidFs.into());
                }
                Ok(Some(crc32c(
                    crc32c(
                        crc32c(seed, &block[..32 + count * 8]),
                        &block[len - 8..len - 4],
                    ),
                    &[0; 4],
                )))
            } else {
                if block[len - 12..len - 4] != [0, 0, 0, 0, 12, 0, 0, 0xde] {
                    return Err(FsError::InvalidFs.into());
                }
                Ok(Some(crc32c(seed, &block[..len - 12])))
            }
        };
        let end = block.len() - 4;
        if let Some(expected) = checksum(&block)?
            && u32_at(&block, end) != expected
        {
            return Err(FsError::InvalidFs.into());
        }
        block[12..16].copy_from_slice(&new.get().to_le_bytes());
        if let Some(crc) = checksum(&block)? {
            block[end..].copy_from_slice(&crc.to_le_bytes());
        }
        if write_at(fs, inode, &block, 0).await? != block.len() {
            return Err(KernelError::Other("short directory write"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_ext4_convention_and_incremental_seed() {
        assert_eq!(crc32c(!0, b"123456789"), !0xe3069283);
        assert_eq!(
            crc32c(crc32c(!0, b"1234"), b"56789"),
            crc32c(!0, b"123456789")
        );
    }
}
