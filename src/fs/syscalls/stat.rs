use super::at::stat::Stat;
use crate::memory::uaccess::copy_to_user;
use crate::{process::fd_table::Fd, sched::current_task};
use libkernel::error::Result;
use libkernel::{error::KernelError, memory::address::TUA};

pub async fn sys_fstat(fd: Fd, statbuf: TUA<Stat>) -> Result<usize> {
    let fd = current_task()
        .fd_table
        .lock_save_irq()
        .get_file(fd)
        .ok_or(KernelError::BadFd)?;

    let inode = fd.inode().ok_or(KernelError::BadFd)?;

    let attr = inode.getattr().await?;

    copy_to_user(statbuf, attr.into()).await?;

    Ok(0)
}
