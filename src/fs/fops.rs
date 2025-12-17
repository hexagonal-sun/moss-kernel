use core::pin::Pin;

use alloc::boxed::Box;
use async_trait::async_trait;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::SeekFrom,
    memory::address::UA,
};

use crate::kernel::kpipe::KPipe;

use super::{dir::OpenFileDirIter, open_file::FileCtx, syscalls::iov::IoVec};

macro_rules! process_iovec {
    ($iovecs:expr, |$addr:ident, $count:ident| $call:expr) => {
        async {
            let mut total_bytes = 0;
            for vec in $iovecs {
                if vec.iov_len == 0 {
                    continue;
                }

                let $addr = vec.iov_base;
                let $count = vec.iov_len;

                let bytes = { $call }.await?;

                total_bytes += bytes;

                if bytes != vec.iov_len {
                    break;
                }
            }

            Ok::<usize, KernelError>(total_bytes)
        }
    };
}

#[async_trait]
pub trait FileOps: Send + Sync {
    /// Reads data from the current file position into `buf`.
    /// The file's cursor is advanced by the number of bytes read.
    async fn read(&mut self, ctx: &mut FileCtx, buf: UA, count: usize) -> Result<usize>;

    /// Writes data from `buf` to the current file position.
    /// The file's cursor is advanced by the number of bytes written.
    async fn write(&mut self, ctx: &mut FileCtx, buf: UA, count: usize) -> Result<usize>;

    async fn readv(&mut self, ctx: &mut FileCtx, iovecs: &[IoVec]) -> Result<usize> {
        process_iovec!(iovecs, |addr, count| self.read(ctx, addr, count)).await
    }

    async fn readdir<'a>(&'a mut self, _ctx: &'a mut FileCtx) -> Result<OpenFileDirIter<'a>> {
        Err(FsError::NotADirectory.into())
    }

    async fn writev(&mut self, ctx: &mut FileCtx, iovecs: &[IoVec]) -> Result<usize> {
        process_iovec!(iovecs, |addr, count| self.write(ctx, addr, count)).await
    }

    /// Puts the current task to sleep until a call to `read()` would no longer
    /// block.
    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        Box::pin(async { Err(KernelError::NotSupported) })
    }

    /// Puts the current task to sleep until a call to `write()` would no longer
    /// block.
    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        Box::pin(async { Err(KernelError::NotSupported) })
    }

    /// Moves the file's cursor to a new position.
    /// Returns the new position from the start of the file.
    async fn seek(&mut self, _ctx: &mut FileCtx, _pos: SeekFrom) -> Result<u64> {
        Err(KernelError::NotSupported)
    }

    /// Performs a device-specific control operation.
    async fn ioctl(&mut self, _ctx: &mut FileCtx, _request: usize, _argp: usize) -> Result<usize> {
        // ENOTTY is the standard error for "ioctl not supported by this file type".
        Err(KernelError::NotATty)
    }

    /// Truncate a file to a specified length
    async fn truncate(&mut self, _ctx: &FileCtx, _new_size: usize) -> Result<()> {
        Err(KernelError::InvalidValue)
    }

    /// Flushes any pending writes to the hardware.
    async fn flush(&self, _ctx: &FileCtx) -> Result<()> {
        Ok(())
    }

    /// Called just before the final reference to the file is going to be
    /// dropped. Allows for any cleanup in an async context.
    async fn release(&mut self, _ctx: &FileCtx) -> Result<()> {
        Ok(())
    }

    async fn splice_into(
        &mut self,
        _ctx: &mut FileCtx,
        _buf: &KPipe,
        _count: usize,
    ) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }

    async fn splice_from(
        &mut self,
        _ctx: &mut FileCtx,
        _buf: &KPipe,
        _count: usize,
    ) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }
}
