use super::{fops::FileOps, open_file::FileCtx};
use crate::memory::uaccess::{copy_from_user_slice, copy_to_user_slice};
use alloc::{boxed::Box, vec};
use async_trait::async_trait;
use core::{cmp::min, future::Future, pin::Pin};
use libkernel::{
    error::{KernelError, Result},
    fs::{BlockDevice, SeekFrom},
    memory::address::UA,
};

pub struct BlockFile {
    device: Box<dyn BlockDevice>,
}

impl BlockFile {
    pub fn new(device: Box<dyn BlockDevice>) -> Self {
        Self { device }
    }
}

#[async_trait]
impl FileOps for BlockFile {
    async fn readat(
        &mut self,
        mut user_buf: UA,
        mut count: usize,
        mut offset: u64,
    ) -> Result<usize> {
        let block_size = self.device.block_size();
        if block_size == 0 {
            return Err(KernelError::InvalidValue);
        }

        let mut total_bytes_read = 0;
        let mut block_buf = vec![0; block_size];

        while count > 0 {
            let block_id = offset / block_size as u64;
            let block_offset = (offset % block_size as u64) as usize;
            let chunk_size = min(count, block_size - block_offset);

            self.device.read(block_id, &mut block_buf).await?;
            copy_to_user_slice(
                &block_buf[block_offset..block_offset + chunk_size],
                user_buf,
            )
            .await?;

            offset += chunk_size as u64;
            count -= chunk_size;
            total_bytes_read += chunk_size;
            user_buf = user_buf.add_bytes(chunk_size);
        }

        Ok(total_bytes_read)
    }

    async fn writeat(
        &mut self,
        mut user_buf: UA,
        mut count: usize,
        mut offset: u64,
    ) -> Result<usize> {
        let block_size = self.device.block_size();
        if block_size == 0 {
            return Err(KernelError::InvalidValue);
        }

        let mut total_bytes_written = 0;
        let mut block_buf = vec![0; block_size];

        while count > 0 {
            let block_id = offset / block_size as u64;
            let block_offset = (offset % block_size as u64) as usize;
            let chunk_size = min(count, block_size - block_offset);

            if block_offset != 0 || chunk_size != block_size {
                self.device.read(block_id, &mut block_buf).await?;
            }

            copy_from_user_slice(
                user_buf,
                &mut block_buf[block_offset..block_offset + chunk_size],
            )
            .await?;
            self.device.write(block_id, &block_buf).await?;

            offset += chunk_size as u64;
            count -= chunk_size;
            total_bytes_written += chunk_size;
            user_buf = user_buf.add_bytes(chunk_size);
        }

        Ok(total_bytes_written)
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        Box::pin(async { Ok(()) })
    }

    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        Box::pin(async { Ok(()) })
    }

    async fn seek(&mut self, ctx: &mut FileCtx, pos: SeekFrom) -> Result<u64> {
        fn saturating_add_signed(value: u64, delta: i64) -> u64 {
            if delta >= 0 {
                value.saturating_add(delta as u64)
            } else {
                value.saturating_sub((-delta) as u64)
            }
        }

        match pos {
            SeekFrom::Start(offset) => ctx.pos = offset,
            SeekFrom::Current(delta) => ctx.pos = saturating_add_signed(ctx.pos, delta),
            SeekFrom::End(_) => return Err(KernelError::NotSupported),
        }

        Ok(ctx.pos)
    }

    async fn flush(&self, _ctx: &FileCtx) -> Result<()> {
        self.device.sync().await
    }
}
