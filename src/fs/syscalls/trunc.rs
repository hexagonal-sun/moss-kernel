use crate::{process::fd_table::Fd, sched::current_task};
use libkernel::{
    error::{KernelError, Result},
};

pub async fn sys_ftruncate(fd: Fd, new_size: usize) -> Result<usize> {
    let fd = current_task()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *fd.lock().await;

    ops.truncate(ctx, new_size).await.map(|_| 0)
}
