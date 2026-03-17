use libkernel::error::{KernelError, Result};

use crate::{fs::VFS, process::fd_table::Fd, sched::syscall_ctx::ProcessCtx};

pub async fn sys_sync(_ctx: &ProcessCtx) -> Result<usize> {
    VFS.sync_all().await?;
    Ok(0)
}

pub async fn sys_syncfs(ctx: &ProcessCtx, fd: Fd) -> Result<usize> {
    let task = ctx.shared().clone();

    let inode = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?
        .inode()
        .ok_or(KernelError::BadFd)?;

    VFS.sync(inode).await?;
    Ok(0)
}

pub async fn sys_fsync(ctx: &ProcessCtx, fd: Fd) -> Result<usize> {
    let task = ctx.shared().clone();

    let inode = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?
        .inode()
        .ok_or(KernelError::BadFd)?;
    inode.sync().await?;

    Ok(0)
}

pub async fn sys_fdatasync(ctx: &ProcessCtx, fd: Fd) -> Result<usize> {
    let task = ctx.shared().clone();

    let inode = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?
        .inode()
        .ok_or(KernelError::BadFd)?;
    inode.datasync().await?;

    Ok(0)
}
