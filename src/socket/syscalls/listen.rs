use crate::process::fd_table::Fd;
use libkernel::error::KernelError;

pub async fn sys_listen(fd: Fd, backlog: i32) -> libkernel::error::Result<usize> {
    let file = crate::sched::current::current_task()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, _ctx) = &mut *file.lock().await;

    ops.as_socket()
        .ok_or(KernelError::NotASocket)?
        .listen(backlog)
        .await?;
    Ok(0)
}
