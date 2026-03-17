use crate::net::ShutdownHow;
use crate::process::fd_table::Fd;
use crate::sched::syscall_ctx::ProcessCtx;

pub async fn sys_shutdown(ctx: &ProcessCtx, fd: Fd, how: i32) -> libkernel::error::Result<usize> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(libkernel::error::KernelError::BadFd)?;

    let (ops, _ctx) = &mut *file.lock().await;

    ops.as_socket()
        .ok_or(libkernel::error::KernelError::NotASocket)?
        .shutdown(ShutdownHow::try_from(how)?)
        .await?;
    Ok(0)
}
