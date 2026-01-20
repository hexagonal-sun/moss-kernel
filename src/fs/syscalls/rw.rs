use crate::{process::fd_table::Fd, sched::current::current_task};
use libkernel::{
    error::{KernelError, Result},
    memory::address::UA,
};

pub async fn sys_write(fd: Fd, user_buf: UA, count: usize) -> Result<usize> {
    if fd.0 == 1 || fd.0 == 2 {
        log::info!("sys_write(fd={}, count={})", fd.0, count);
    }

    let file = current_task()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    let res = ops.write(ctx, user_buf, count).await;
    if fd.0 == 1 || fd.0 == 2 {
        log::info!("sys_write(fd={}) -> {:?}", fd.0, res);
    }
    res
}

pub async fn sys_read(fd: Fd, user_buf: UA, count: usize) -> Result<usize> {
    let file = current_task()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *file.lock().await;

    ops.read(ctx, user_buf, count).await
}

pub async fn sys_pwrite64(fd: Fd, user_buf: UA, count: usize, offset: u64) -> Result<usize> {
    let file = current_task()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, _ctx) = &mut *file.lock().await;

    ops.writeat(user_buf, count, offset).await
}

pub async fn sys_pread64(fd: Fd, user_buf: UA, count: usize, offset: u64) -> Result<usize> {
    let file = current_task()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, _ctx) = &mut *file.lock().await;

    ops.readat(user_buf, count, offset).await
}
