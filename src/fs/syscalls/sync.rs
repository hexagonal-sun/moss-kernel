use libkernel::error::{KernelError, Result};

use crate::{fs::VFS, process::fd_table::Fd, sched::current_task};

pub async fn sys_sync() -> Result<usize> {
    VFS.sync_all().await?;
    Ok(0)
}

pub async fn sys_syncfs(fd: Fd) -> Result<usize> {
    let task = current_task();

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

pub async fn sys_fsync(fd: Fd) -> Result<usize> {
    let task = current_task();

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

pub async fn sys_fdatasync(fd: Fd) -> Result<usize> {
    let task = current_task();

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
