use super::{fops::FileOps, open_file::FileCtx};
use crate::{
    kernel::kpipe::KPipe,
    memory::{
        page::ClaimedPage,
        uaccess::{copy_from_user_slice, copy_to_user_slice},
    },
};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use core::{cmp::min, pin::Pin};
use libkernel::{
    error::Result,
    fs::{Inode, SeekFrom},
    memory::{PAGE_SIZE, address::UA},
};

const SPLICE_BUF_SZ: usize = 32;

pub struct RegFile {
    inode: Arc<dyn Inode>,
}

impl RegFile {
    pub fn new(inode: Arc<dyn Inode>) -> Self {
        Self { inode }
    }
}

#[async_trait]
impl FileOps for RegFile {
    /// Reads data from the current file position into `buf`. The file's cursor
    /// is advanced by the number of bytes read.
    async fn read(
        &mut self,
        ctx: &mut FileCtx,
        mut user_buf: UA,
        mut count: usize,
    ) -> Result<usize> {
        let mut pg = ClaimedPage::alloc_zeroed()?;
        let kbuf = pg.as_slice_mut();
        let mut total_bytes_read = 0;

        while count > 0 {
            let chunk_sz = min(PAGE_SIZE, count);
            copy_from_user_slice(user_buf, &mut kbuf[..chunk_sz]).await?;

            let bytes_read = self.inode.read_at(ctx.pos, &mut kbuf[..chunk_sz]).await?;

            if bytes_read == 0 {
                break;
            }

            copy_to_user_slice(&kbuf[..bytes_read], user_buf).await?;

            ctx.pos += bytes_read as u64;
            total_bytes_read += bytes_read;
            user_buf = user_buf.add_bytes(bytes_read);
            count -= bytes_read;
        }

        Ok(total_bytes_read)
    }

    /// Writes data from `buf` to the current file position.
    /// The file's cursor is advanced by the number of bytes written.
    async fn write(&mut self, ctx: &mut FileCtx, mut buf: UA, mut count: usize) -> Result<usize> {
        let mut pg = ClaimedPage::alloc_zeroed()?;
        let kbuf = pg.as_slice_mut();
        let mut total_bytes_written = 0;

        while count > 0 {
            let chunk_sz = min(PAGE_SIZE, count);

            copy_from_user_slice(buf, &mut kbuf[..chunk_sz]).await?;

            let bytes_written = self.inode.write_at(ctx.pos, &kbuf[..chunk_sz]).await?;

            // If we wrote 0 bytes, the disk might be full or the file cannot be
            // extended.
            if bytes_written == 0 {
                break;
            }

            ctx.pos += bytes_written as u64;
            total_bytes_written += bytes_written;
            count -= bytes_written;
            buf = buf.add_bytes(bytes_written);
        }

        Ok(total_bytes_written)
    }

    async fn truncate(&mut self, _ctx: &FileCtx, new_size: usize) -> Result<()> {
        self.inode.truncate(new_size as _).await
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        // For regular files, polling just returns ready.
        Box::pin(async { Ok(()) })
    }

    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        // For regular files, polling just returns ready.
        Box::pin(async { Ok(()) })
    }

    /// Moves the file's cursor to a new position.
    /// Returns the new position from the start of the file.
    async fn seek(&mut self, ctx: &mut FileCtx, pos: SeekFrom) -> Result<u64> {
        let size = self.inode.getattr().await?.size;

        fn saturating_add_signed(u: u64, i: i64) -> u64 {
            if i >= 0 {
                u.saturating_add(i as u64)
            } else {
                u.saturating_sub((-i) as u64)
            }
        }

        match pos {
            SeekFrom::Start(x) => ctx.pos = x,
            SeekFrom::End(x) => ctx.pos = saturating_add_signed(size, x),
            SeekFrom::Current(x) => ctx.pos = saturating_add_signed(ctx.pos, x),
        }

        Ok(ctx.pos)
    }

    async fn splice_into(
        &mut self,
        ctx: &mut FileCtx,
        kbuf: &KPipe,
        count: usize,
    ) -> Result<usize> {
        let mut buf = [0; SPLICE_BUF_SZ];

        let bytes_read = self
            .inode
            .read_at(ctx.pos, &mut buf[..min(SPLICE_BUF_SZ, count)])
            .await?;

        if bytes_read == 0 {
            return Ok(0);
        }

        ctx.pos += bytes_read as u64;

        let mut data_to_write = &buf[..bytes_read];

        while !data_to_write.is_empty() {
            let written = kbuf.push_slice(data_to_write).await;
            data_to_write = &data_to_write[written..];
        }

        Ok(bytes_read)
    }
}
