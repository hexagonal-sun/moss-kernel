use crate::{process::fd_table::Fd, sched::current_task};
use libkernel::{
    error::{KernelError, Result},
    memory::address::UA,
};

pub async fn sys_write(fd: Fd, user_buf: UA, count: usize) -> Result<usize> {
    let file = current_task()
        .fd_table
        .lock_save_irq()
        .get_file(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    ops.write(ctx, user_buf, count).await
}

pub async fn sys_read(fd: Fd, user_buf: UA, count: usize) -> Result<usize> {
    let file = current_task()
        .fd_table
        .lock_save_irq()
        .get_file(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    ops.read(ctx, user_buf, count).await
}
