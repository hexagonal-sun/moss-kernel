use crate::{process::fd_table::Fd, sched::syscall_ctx::ProcessCtx};
use libkernel::{
    error::{KernelError, Result},
    memory::address::UA,
};

pub async fn sys_write(ctx: &ProcessCtx, fd: Fd, user_buf: UA, count: usize) -> Result<usize> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    ctx.require_writable()?;
    ops.write(ctx, user_buf, count).await
}

pub async fn sys_read(ctx: &ProcessCtx, fd: Fd, user_buf: UA, count: usize) -> Result<usize> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    ctx.require_readable()?;
    ops.read(ctx, user_buf, count).await
}

pub async fn sys_pwrite64(
    ctx: &ProcessCtx,
    fd: Fd,
    user_buf: UA,
    count: usize,
    offset: u64,
) -> Result<usize> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    ctx.require_writable()?;
    ops.writeat(user_buf, count, offset).await
}

pub async fn sys_pread64(
    ctx: &ProcessCtx,
    fd: Fd,
    user_buf: UA,
    count: usize,
    offset: u64,
) -> Result<usize> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    ctx.require_readable()?;
    ops.readat(user_buf, count, offset).await
}
