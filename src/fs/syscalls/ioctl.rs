use crate::{process::fd_table::Fd, sched::syscall_ctx::ProcessCtx};
use libkernel::error::{KernelError, Result};

pub async fn sys_ioctl(ctx: &ProcessCtx, fd: Fd, request: usize, arg: usize) -> Result<usize> {
    let fd = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *fd.lock().await;
    ops.ioctl(ctx, request, arg).await
}
