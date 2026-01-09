use crate::fs::VFS;
use crate::memory::uaccess::cstr::UserCStr;
use crate::process::fd_table::Fd;
use crate::sched::current::current_task_shared;
use alloc::sync::Arc;
use core::ffi::c_char;
use libkernel::error::{KernelError, Result};
use libkernel::fs::Inode;
use libkernel::fs::path::Path;
use libkernel::memory::address::TUA;

async fn removexattr(node: Arc<dyn Inode>, name: &str) -> Result<()> {
    node.removexattr(name).await?;
    Ok(())
}

pub async fn sys_removexattr(path: TUA<c_char>, name: TUA<c_char>) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = current_task_shared();

    let node = VFS.resolve_path(path, VFS.root_inode(), &task).await?;
    let mut buf = [0; 1024];
    removexattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
    )
    .await?;
    Ok(0)
}

pub async fn sys_lremovexattr(path: TUA<c_char>, name: TUA<c_char>) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = current_task_shared();

    let node = VFS
        .resolve_path_nofollow(path, VFS.root_inode(), &task)
        .await?;
    let mut buf = [0; 1024];
    removexattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
    )
    .await?;
    Ok(0)
}

pub async fn sys_fremovexattr(fd: Fd, name: TUA<c_char>) -> Result<usize> {
    let node = {
        let task = current_task_shared();
        let file = task
            .fd_table
            .lock_save_irq()
            .get(fd)
            .ok_or(KernelError::BadFd)?;

        file.inode().ok_or(KernelError::BadFd)?
    };
    let mut buf = [0; 1024];
    removexattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
    )
    .await?;
    Ok(0)
}
