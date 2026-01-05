use crate::fs::fops::FileOps;
use crate::fs::open_file::FileCtx;
use crate::socket::{ShutdownHow, SockAddr};
use alloc::boxed::Box;
use async_trait::async_trait;
use libkernel::error::KernelError;
use libkernel::memory::address::UA;

#[async_trait]
pub trait SocketOps: Send + Sync {
    async fn bind(&self, _addr: SockAddr) -> libkernel::error::Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn connect(&self, _addr: SockAddr) -> libkernel::error::Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn listen(&self, _backlog: i32) -> libkernel::error::Result<()> {
        Err(KernelError::NotSupported)
    }

    async fn accept(&self) -> libkernel::error::Result<Box<dyn SocketOps>> {
        Err(KernelError::NotSupported)
    }

    async fn read(
        &mut self,
        ctx: &mut FileCtx,
        buf: UA,
        count: usize,
    ) -> libkernel::error::Result<usize>;
    async fn write(
        &mut self,
        ctx: &mut FileCtx,
        buf: UA,
        count: usize,
    ) -> libkernel::error::Result<usize>;

    async fn shutdown(&self, _how: ShutdownHow) -> libkernel::error::Result<()> {
        Err(KernelError::NotSupported)
    }

    fn as_file(self: Box<Self>) -> Box<dyn FileOps>;
}

#[async_trait]
impl<T> FileOps for T
where
    T: SocketOps,
{
    async fn read(
        &mut self,
        ctx: &mut FileCtx,
        buf: UA,
        count: usize,
    ) -> libkernel::error::Result<usize> {
        self.read(ctx, buf, count).await
    }

    async fn readat(
        &mut self,
        _buf: UA,
        _count: usize,
        _offset: u64,
    ) -> libkernel::error::Result<usize> {
        Err(KernelError::NotSupported)
    }

    async fn write(
        &mut self,
        ctx: &mut FileCtx,
        buf: UA,
        count: usize,
    ) -> libkernel::error::Result<usize> {
        self.write(ctx, buf, count).await
    }

    async fn writeat(
        &mut self,
        _buf: UA,
        _count: usize,
        _offset: u64,
    ) -> libkernel::error::Result<usize> {
        Err(KernelError::NotSupported)
    }

    fn is_socket(&self) -> bool {
        true
    }

    fn as_socket(&mut self) -> Option<&mut dyn SocketOps> {
        Some(self)
    }
}
