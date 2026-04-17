use alloc::boxed::Box;
use async_trait::async_trait;
use core::pin::Pin;
use libkernel::{
    error::{KernelError, Result},
    fs::SeekFrom,
    memory::address::UA,
};

use crate::kernel::kpipe::KPipe;

use super::{dir::OpenFileDirIter, fops::FileOps, open_file::FileCtx};

/// A file object created for descriptors opened with `O_PATH`.
///
/// These descriptors may be used for metadata and pathname-based operations,
/// but data I/O on them should fail with `EBADF`.
pub struct PathOnlyFile;

#[async_trait]
impl FileOps for PathOnlyFile {
    async fn read(&mut self, _ctx: &mut FileCtx, _buf: UA, _count: usize) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn write(&mut self, _ctx: &mut FileCtx, _buf: UA, _count: usize) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn readdir<'a>(&'a mut self, _ctx: &'a mut FileCtx) -> Result<OpenFileDirIter<'a>> {
        Err(KernelError::BadFd)
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        Box::pin(async { Err(KernelError::BadFd) })
    }

    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        Box::pin(async { Err(KernelError::BadFd) })
    }

    async fn seek(&mut self, _ctx: &mut FileCtx, _pos: SeekFrom) -> Result<u64> {
        Err(KernelError::BadFd)
    }

    async fn ioctl(&mut self, _ctx: &mut FileCtx, _request: usize, _argp: usize) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn truncate(&mut self, _ctx: &FileCtx, _new_size: usize) -> Result<()> {
        Err(KernelError::BadFd)
    }

    async fn flush(&self, _ctx: &FileCtx) -> Result<()> {
        Err(KernelError::BadFd)
    }

    async fn splice_into(
        &mut self,
        _ctx: &mut FileCtx,
        _buf: &KPipe,
        _count: usize,
    ) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn splice_from(
        &mut self,
        _ctx: &mut FileCtx,
        _buf: &KPipe,
        _count: usize,
    ) -> Result<usize> {
        Err(KernelError::BadFd)
    }
}
