use crate::net::sops::SendFlags;
use crate::net::{SocketLen, parse_sockaddr};
use crate::process::fd_table::Fd;
use crate::sched::syscall_ctx::ProcessCtx;
use libkernel::error::Result;
use libkernel::memory::address::UA;

// pub async fn sys_send(fd: Fd, buf: UA, len: usize, flags: i32) -> Result<usize> {
//     let file = crate::sched::current::current_task()
//         .fd_table
//         .lock_save_irq()
//         .get(fd)
//         .ok_or(libkernel::error::KernelError::BadFd)?;
//     if flags != 0 {
//         log::warn!("sys_send: flags parameter is not supported yet: {}", flags);
//     }
//
//     let (ops, ctx) = &mut *file.lock().await;
//     let socket = ops
//         .as_socket()
//         .ok_or(libkernel::error::KernelError::NotASocket)?;
//     let flags = SendFlags::from_bits(flags as u32).unwrap_or(SendFlags::empty());
//     socket.send(ctx, buf, len, flags).await
// }

const MSG_NOSIGNAL: i32 = 0x4000;

pub async fn sys_sendto(
    ctx: &ProcessCtx,
    fd: Fd,
    buf: UA,
    len: usize,
    flags: i32,
    addr: UA,
    addrlen: SocketLen,
) -> Result<usize> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(libkernel::error::KernelError::BadFd)?;
    if flags != 0 && flags != MSG_NOSIGNAL {
        log::warn!("sys_sendto: flags parameter is not supported yet: {flags}");
    }

    let (ops, ctx) = &mut *file.lock().await;
    let socket = ops
        .as_socket()
        .ok_or(libkernel::error::KernelError::NotASocket)?;
    let flags = SendFlags::from_bits(flags as u32).unwrap_or(SendFlags::empty());
    if addr.is_null() || addrlen == 0 {
        // No destination address, use connected peer
        return socket.send(ctx, buf, len, flags).await;
    }
    let addr = parse_sockaddr(addr, addrlen).await?;
    socket.sendto(ctx, buf, len, flags, addr).await
}
