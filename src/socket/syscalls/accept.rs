use crate::fs::open_file::OpenFile;
use crate::process::fd_table::Fd;
use crate::sched::current::current_task_shared;
use libkernel::error::KernelError;
use libkernel::fs::OpenFlags;

pub async fn sys_accept(fd: Fd) -> libkernel::error::Result<usize> {
    let file = current_task_shared()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, _ctx) = &mut *file.lock().await;

    let new_socket = ops
        .as_socket()
        .ok_or(KernelError::NotASocket)?
        .accept()
        .await?
        .as_file();

    let open_file = OpenFile::new(new_socket, OpenFlags::empty());
    let new_fd = current_task_shared()
        .fd_table
        .lock_save_irq()
        .insert(alloc::sync::Arc::new(open_file))?;
    Ok(new_fd.as_raw() as usize)
}
