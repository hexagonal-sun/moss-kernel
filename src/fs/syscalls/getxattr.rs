use crate::fs::VFS;
use crate::memory::uaccess::copy_to_user_slice;
use crate::memory::uaccess::cstr::UserCStr;
use crate::process::fd_table::Fd;
use crate::sched::current::current_task_shared;
use alloc::sync::Arc;
use core::ffi::c_char;
use libkernel::error::{KernelError, Result};
use libkernel::fs::Inode;
use libkernel::fs::path::Path;
use libkernel::memory::address::{TUA, UA};

async fn getxattr(node: Arc<dyn Inode>, name: &str, ua: UA, size: usize) -> Result<usize> {
    let value = node.getxattr(name).await?;
    if size < value.len() {
        Err(KernelError::RangeError)
    } else {
        copy_to_user_slice(&value, ua).await?;
        Ok(value.len())
    }
}

pub async fn sys_getxattr(
    path: TUA<c_char>,
    name: TUA<c_char>,
    value: UA,
    size: usize,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = current_task_shared();

    let node = VFS.resolve_path(path, VFS.root_inode(), &task).await?;
    let mut buf = [0; 1024];
    getxattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
        value,
        size,
    )
    .await
}

pub async fn sys_lgetxattr(
    path: TUA<c_char>,
    name: TUA<c_char>,
    value: UA,
    size: usize,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = current_task_shared();

    let node = VFS
        .resolve_path_nofollow(path, VFS.root_inode(), &task)
        .await?;
    let mut buf = [0; 1024];
    getxattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
        value,
        size,
    )
    .await
}

pub async fn sys_fgetxattr(fd: Fd, name: TUA<c_char>, value: UA, size: usize) -> Result<usize> {
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
    getxattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
        value,
        size,
    )
    .await
}
