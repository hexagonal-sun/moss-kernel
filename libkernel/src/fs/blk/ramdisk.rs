//! RAM-backed block device implementation.

use crate::{
    KernAddressSpace,
    error::{IoError, KernelError, Result},
    fs::BlockDevice,
    memory::{
        PAGE_SIZE,
        address::{TVA, VA},
        permissions::PtePermissions,
        region::{PhysMemoryRegion, VirtMemoryRegion},
    },
};
use alloc::boxed::Box;
use async_trait::async_trait;
use core::ptr;

/// A block device backed by a region of RAM.
pub struct RamdiskBlkDev {
    base: TVA<u8>,
    num_blocks: u64,
}

const BLOCK_SIZE: usize = PAGE_SIZE;

impl RamdiskBlkDev {
    /// Creates a new ramdisk.
    ///
    /// Maps the given physical memory region into the kernel's address space at
    /// the specified virtual base address.
    pub fn new<K: KernAddressSpace>(
        region: PhysMemoryRegion,
        base: VA,
        kern_addr_spc: &mut K,
    ) -> Result<Self> {
        kern_addr_spc.map_normal(
            region,
            VirtMemoryRegion::new(base, region.size()),
            PtePermissions::rw(false),
        )?;

        if !region.size().is_multiple_of(BLOCK_SIZE) {
            return Err(KernelError::InvalidValue);
        }

        let num_blocks = (region.size() / BLOCK_SIZE) as u64;

        Ok(Self {
            base: TVA::from_value(base.value()),
            num_blocks,
        })
    }
}

#[async_trait]
impl BlockDevice for RamdiskBlkDev {
    /// Read one or more blocks starting at `block_id`.
    /// The `buf` length must be a multiple of `block_size`.
    async fn read(&self, block_id: u64, buf: &mut [u8]) -> Result<()> {
        debug_assert!(buf.len().is_multiple_of(BLOCK_SIZE));

        let num_blocks_to_read = (buf.len() / BLOCK_SIZE) as u64;

        // Ensure the read doesn't go past the end of the ramdisk.
        if block_id + num_blocks_to_read > self.num_blocks {
            return Err(IoError::OutOfBounds.into());
        }

        let offset = block_id as usize * BLOCK_SIZE;

        unsafe {
            // SAFETY: VA can be accessed:
            //
            // 1. We have successfully mapped the ramdisk into virtual memory,
            //    starting at base.
            // 2. We have bounds checked the access.
            let src_ptr = self.base.as_ptr().add(offset);

            ptr::copy_nonoverlapping(src_ptr, buf.as_mut_ptr(), buf.len());
        }

        Ok(())
    }

    /// Write one or more blocks starting at `block_id`.
    /// The `buf` length must be a multiple of `block_size`.
    async fn write(&self, block_id: u64, buf: &[u8]) -> Result<()> {
        debug_assert!(buf.len().is_multiple_of(BLOCK_SIZE));

        let num_blocks_to_write = (buf.len() / BLOCK_SIZE) as u64;

        if block_id + num_blocks_to_write > self.num_blocks {
            return Err(IoError::OutOfBounds.into());
        }

        let offset = block_id as usize * BLOCK_SIZE;

        unsafe {
            let dest_ptr = self.base.as_ptr_mut().add(offset);

            ptr::copy_nonoverlapping(buf.as_ptr(), dest_ptr, buf.len());
        }

        Ok(())
    }

    /// The size of a single block in bytes.
    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    /// Flushes any caches to the underlying device.
    async fn sync(&self) -> Result<()> {
        Ok(())
    }
}
