use crate::memory::uaccess::{copy_from_user, copy_to_user, copy_to_user_slice};
use crate::net::sops::RecvFlags;
use crate::net::{SocketLen, parse_sockaddr};
use crate::process::fd_table::Fd;
use crate::sched::syscall_ctx::ProcessCtx;
use libkernel::error::KernelError;
use libkernel::memory::address::{TUA, UA};

pub async fn sys_recvfrom(
    ctx: &ProcessCtx,
    fd: Fd,
    buf: UA,
    len: usize,
    flags: i32,
    addr: UA,
    addrlen: TUA<SocketLen>,
) -> libkernel::error::Result<usize> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;
    if flags != 0 {
        log::warn!("sys_recvfrom: flags parameter is not supported yet: {flags}");
    }

    let (ops, ctx) = &mut *file.lock().await;
    let socket = ops.as_socket().ok_or(KernelError::NotASocket)?;
    let flags = RecvFlags::from_bits(flags as u32).unwrap_or(RecvFlags::empty());
    let socket_addr = if !addr.is_null() {
        let addrlen_val = copy_from_user(addrlen).await?;
        Some(parse_sockaddr(addr, addrlen_val).await?)
    } else {
        None
    };
    let (message_len, recv_addr) = socket.recvfrom(ctx, buf, len, flags, socket_addr).await?;
    if let Some(recv_addr) = recv_addr
        && addr.is_null()
    {
        if addrlen.is_null() {
            return Err(KernelError::InvalidValue);
        }
        let addrlen_val = copy_from_user(addrlen).await?;
        let bytes = recv_addr.to_bytes();
        let to_copy = bytes.len().min(addrlen_val);
        copy_to_user_slice(&bytes[..to_copy], addr).await?;
        copy_to_user(addrlen, bytes.len()).await?;
    }
    Ok(message_len)
}
