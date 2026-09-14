//! A path reference with no file I/O operations or device-open side effects.
use super::fops::FileOps;
use alloc::boxed::Box;
use async_trait::async_trait;
use libkernel::{
    error::{KernelError, Result},
    memory::address::UA,
};

pub struct PathFile;

#[async_trait]
impl FileOps for PathFile {
    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }
}
