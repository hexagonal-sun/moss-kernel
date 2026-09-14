use libkernel::error::{KernelError, Result};

use crate::{
    process::{fd_table::Fd, inotify::notify_attrib},
    sched::syscall_ctx::ProcessCtx,
};

pub async fn sys_fchown(ctx: &ProcessCtx, fd: Fd, owner: i32, group: i32) -> Result<usize> {
    let task = ctx.shared().clone();
    let file = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let inode = file.inode().ok_or(KernelError::BadFd)?;
    let mut attr = inode.getattr().await?;

    task.creds.lock_save_irq().chown(&mut attr, owner, group)?;
    inode.setattr(attr).await?;
    notify_attrib(inode.id()).await;

    Ok(0)
}
