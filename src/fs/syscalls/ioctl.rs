use crate::{process::fd_table::Fd, sched::current_task};
use libkernel::error::{KernelError, Result};

pub async fn sys_ioctl(fd: Fd, request: usize, arg: usize) -> Result<usize> {
    let fd = current_task()
        .fd_table
        .lock_save_irq()
        .get_file(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *fd.lock().await;
    ops.ioctl(ctx, request, arg).await
}
