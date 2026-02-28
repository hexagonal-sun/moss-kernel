use crate::process::fd_table::Fd;
use crate::socket::ShutdownHow;

pub async fn sys_shutdown(fd: Fd, how: i32) -> libkernel::error::Result<usize> {
    let file = crate::sched::current::current_task()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(libkernel::error::KernelError::BadFd)?;

    let (ops, _ctx) = &mut *file.lock().await;

    ops.as_socket()
        .ok_or(libkernel::error::KernelError::NotASocket)?
        .shutdown(ShutdownHow::try_from(how)?)
        .await?;
    Ok(0)
}
