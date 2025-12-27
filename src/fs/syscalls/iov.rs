use crate::{
    memory::uaccess::{UserCopyable, copy_obj_array_from_user},
    process::fd_table::Fd,
    sched::current_task,
};
use libkernel::{
    error::{KernelError, Result},
    memory::address::{TUA, UA},
};

#[derive(Clone, Copy)]
#[repr(C)]
pub struct IoVec {
    pub iov_base: UA,
    pub iov_len: usize,
}

// SAFETY: An IoVec is safe to copy to-and-from userspace.
unsafe impl UserCopyable for IoVec {}

pub async fn sys_writev(fd: Fd, iov_ptr: TUA<IoVec>, no_iov: usize) -> Result<usize> {
    let file = current_task()
        .fd_table
        .lock_save_irq()
        .get_file(fd)
        .ok_or(KernelError::BadFd)?;

    let iovs = copy_obj_array_from_user(iov_ptr, no_iov).await?;

    let (ops, state) = &mut *file.lock().await;

    ops.writev(state, &iovs).await
}

pub async fn sys_readv(fd: Fd, iov_ptr: TUA<IoVec>, no_iov: usize) -> Result<usize> {
    let file = current_task()
        .fd_table
        .lock_save_irq()
        .get_file(fd)
        .ok_or(KernelError::BadFd)?;

    let iovs = copy_obj_array_from_user(iov_ptr, no_iov).await?;

    let (ops, state) = &mut *file.lock().await;

    ops.readv(state, &iovs).await
}
