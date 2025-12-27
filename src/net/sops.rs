use crate::net::open_socket::SocketCtx;
use alloc::boxed::Box;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::error::{KernelError, Result};

#[async_trait]
pub trait SocketOps: Send + Sync {
    async fn bind(&self, _ctx: &mut SocketCtx, _addr: &[u8]) -> Result<()> {
        Err(KernelError::NotSupported)
    }
    async fn connect(&self, _ctx: &mut SocketCtx, _addr: &[u8]) -> Result<()> {
        Err(KernelError::NotSupported)
    }
    async fn listen(&self, _ctx: &mut SocketCtx, _backlog: i32) -> Result<()> {
        Err(KernelError::NotSupported)
    }
    async fn accept(&self, _ctx: &mut SocketCtx) -> Result<Box<dyn SocketOps>> {
        Err(KernelError::NotSupported)
    }
    async fn sendmsg(&self, _ctx: &mut SocketCtx, _msg: &[u8]) -> Result<usize> {
        Err(KernelError::NotSupported)
    }
    async fn recvmsg(&self, _ctx: &mut SocketCtx, _buf: &mut [u8]) -> Result<usize> {
        Err(KernelError::NotSupported)
    }
    fn getsockopt(&self, _ctx: &mut SocketCtx, _level: i32, _opt: i32) -> Result<Vec<u8>> {
        Err(KernelError::NotSupported)
    }
    fn setsockopt(&self, _ctx: &mut SocketCtx, _level: i32, _opt: i32, _val: &[u8]) -> Result<()> {
        Err(KernelError::NotSupported)
    }
}
