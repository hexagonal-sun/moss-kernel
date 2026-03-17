use crate::fs::open_file::OpenFile;
use crate::memory::uaccess::{copy_from_user, copy_to_user, copy_to_user_slice};
use crate::net::SocketLen;
use crate::process::fd_table::Fd;
use crate::sched::syscall_ctx::ProcessCtx;
use libkernel::error::KernelError;
use libkernel::fs::OpenFlags;
use libkernel::memory::address::{TUA, UA};

pub async fn sys_accept4(
    ctx: &ProcessCtx,
    fd: Fd,
    addr: UA,
    addrlen: TUA<SocketLen>,
    _flags: i32,
) -> libkernel::error::Result<usize> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, _ctx) = &mut *file.lock().await;

    let (new_socket, socket_addr) = ops
        .as_socket()
        .ok_or(KernelError::NotASocket)?
        .accept()
        .await?;
    let new_socket = new_socket.as_file();

    let open_file = OpenFile::new(new_socket, OpenFlags::empty());
    let new_fd = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .insert(alloc::sync::Arc::new(open_file))?;
    if !addr.is_null() {
        if addrlen.is_null() {
            return Err(KernelError::InvalidValue);
        }
        let addrlen_val = copy_from_user(addrlen).await?;
        let bytes = socket_addr.to_bytes();
        let to_copy = bytes.len().min(addrlen_val);
        copy_to_user_slice(&bytes[..to_copy], addr).await?;
        copy_to_user(addrlen, bytes.len()).await?;
    }
    Ok(new_fd.as_raw() as usize)
}

pub async fn sys_accept(
    ctx: &ProcessCtx,
    fd: Fd,
    addr: UA,
    addrlen: TUA<SocketLen>,
) -> libkernel::error::Result<usize> {
    sys_accept4(ctx, fd, addr, addrlen, 0).await
}
