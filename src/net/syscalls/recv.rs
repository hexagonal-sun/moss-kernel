use crate::process::fd_table::Fd;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::UA;

async fn recv(
    _fd: Fd,
    _buf: UA,
    _size: usize,
    _flags: i32,
    _src_addr: Option<UA>,
    _addrlen: Option<u32>,
) -> Result<usize> {
    Err(KernelError::NotSupported)
}

#[allow(dead_code)]
pub async fn sys_recv(fd: Fd, buf: UA, size: usize, flags: i32) -> Result<usize> {
    recv(fd, buf, size, flags, None, None).await
}

pub async fn sys_recvfrom(
    fd: Fd,
    buf: UA,
    size: usize,
    flags: i32,
    src_addr: UA,
    addrlen: u32,
) -> Result<usize> {
    recv(fd, buf, size, flags, Some(src_addr), Some(addrlen)).await
}
