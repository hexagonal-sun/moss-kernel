use core::{mem, slice};

use crate::{CpuOps, error::Result, fs::BlockDevice, pod::Pod};

use crate::fs::blk::block_cache::BlockCache;
use crate::sync::mutex::Mutex;
use alloc::{boxed::Box, vec};

/// A buffer that provides byte-level access to an underlying BlockDevice.
///
/// This layer handles the logic of translating byte offsets and lengths into
/// block-based operations, including handling requests that span multiple
/// blocks or are not aligned to block boundaries.
///
/// TODO: Cache blocks.
pub struct BlockBuffer<CPU: CpuOps> {
    // TODO: Change to rwlock when we have one.
    // This would require a bit of rearchitecture to maximize read sharing.
    cache: Mutex<BlockCache, CPU>,
    block_size: usize,
}

impl<CPU> BlockBuffer<CPU>
where
    CPU: CpuOps,
{
    /// Creates a new `BlockBuffer` that wraps the given block device.
    pub fn new(dev: Box<dyn BlockDevice>) -> Self {
        let block_size = dev.block_size();

        Self {
            cache: Mutex::new(BlockCache::new(64, block_size, dev)),
            block_size,
        }
    }

    /// Reads a sequence of bytes starting at a specific offset.
    pub async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let len = buf.len();

        if len == 0 {
            return Ok(());
        }

        let start_block = offset / self.block_size as u64;
        let end_offset = offset + len as u64;

        let end_block = (end_offset - 1) / self.block_size as u64;

        let num_blocks_to_read = end_block - start_block + 1;

        let mut temp_buf = vec![0; num_blocks_to_read as usize * self.block_size];

        let mut cache = self.cache.lock().await;
        for block_index in 0..num_blocks_to_read {
            let block_number = start_block + block_index;
            let block_data = cache.get_or_load(block_number).await?;
            let start = (block_index as usize) * self.block_size;
            let end = start + self.block_size;
            temp_buf[start..end].copy_from_slice(block_data);
        }

        let start_in_temp_buf = (offset % self.block_size as u64) as usize;
        let end_in_temp_buf = start_in_temp_buf + len;

        buf.copy_from_slice(&temp_buf[start_in_temp_buf..end_in_temp_buf]);

        Ok(())
    }

    /// Reads a `Pod` struct directly from the device at a given offset.
    pub async fn read_obj<T: Pod>(&self, offset: u64) -> Result<T> {
        let mut dest = mem::MaybeUninit::<T>::uninit();

        // SAFETY: We create a mutable byte slice that points to our
        // uninitialized stack space. This is safe because:
        // 1. The pointer is valid and properly aligned for T.
        // 2. The size is correct for T.
        // 3. We are only writing to this slice, not reading from it yet.
        let buf: &mut [u8] =
            unsafe { slice::from_raw_parts_mut(dest.as_mut_ptr() as *mut u8, mem::size_of::<T>()) };

        // Read directly from the device into our stack-allocated space.
        self.read_at(offset, buf).await?;

        // SAFETY: The `read_at` call has now filled the buffer with bytes from
        // the device. Since `T` is `Pod`, any combination of bytes is a valid
        // `T`, so we can now safely assume it is initialized.
        Ok(unsafe { dest.assume_init() })
    }

    /// Writes a sequence of bytes starting at a specific offset.
    ///
    /// NOTE: This is a simple but potentially inefficient implementation that
    /// uses a read-modify-write approach for all writes.
    pub async fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }

        let start_block = offset / self.block_size as u64;
        let end_offset = offset + len as u64;
        let end_block = (end_offset - 1) / self.block_size as u64;

        let num_blocks_to_rw = end_block - start_block + 1;
        let mut temp_buf = vec![0; num_blocks_to_rw as usize * self.block_size];

        let mut cache = self.cache.lock().await;
        for block_index in 0..num_blocks_to_rw {
            let block_number = start_block + block_index;
            let block_data = cache.get_or_load_mut(block_number).await?;
            let start = (block_index as usize) * self.block_size;
            let end = start + self.block_size;
            temp_buf[start..end].copy_from_slice(block_data);
        }
        // // Read all affected blocks from the device into our temporary buffer.
        // // This preserves the data in the blocks that we are not modifying.
        // self.dev.read(start_block, &mut temp_buf).await?;
        //
        // // Copy the user's data into the correct position in our temporary
        // // buffer.
        // let start_in_temp_buf = (offset % self.block_size as u64) as usize;
        // let end_in_temp_buf = start_in_temp_buf + len;
        //
        // temp_buf[start_in_temp_buf..end_in_temp_buf].copy_from_slice(buf);
        //
        // // Write the entire modified buffer back to the device.
        // self.dev.write(start_block, &temp_buf).await?;

        Ok(())
    }

    /// Forwards a sync call to the underlying device.
    pub async fn sync(&self) -> Result<()> {
        self.cache.lock().await.write_dirty().await?;
        self.cache.lock().await.dev.sync().await
    }
}
