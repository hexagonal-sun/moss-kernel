use crate::process::fd_table::Fd;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::UA;

async fn send(
    _fd: Fd,
    _buf: UA,
    _size: usize,
    _flags: i32,
    _dest_addr: Option<UA>,
    _addrlen: Option<u32>,
) -> Result<usize> {
    Err(KernelError::NotSupported)
}

#[allow(dead_code)]
pub async fn sys_send(fd: Fd, buf: UA, size: usize, flags: i32) -> Result<usize> {
    send(fd, buf, size, flags, None, None).await
}

pub async fn sys_sendto(
    fd: Fd,
    buf: UA,
    size: usize,
    flags: i32,
    dest_addr: UA,
    addrlen: u32,
) -> Result<usize> {
    send(fd, buf, size, flags, Some(dest_addr), Some(addrlen)).await
}
