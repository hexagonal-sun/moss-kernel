use crate::fs::VFS;
use crate::memory::uaccess::cstr::UserCStr;
use crate::process::fd_table::Fd;
use crate::sched::current::current_task_shared;
use alloc::sync::Arc;
use core::ffi::c_char;
use libkernel::error::{KernelError, Result};
use libkernel::fs::Inode;
use libkernel::fs::path::Path;
use libkernel::memory::address::{TUA, UA};

fn setxattr(
    _node: Arc<dyn Inode>,
    name: &str,
    _value: UA,
    size: usize,
    _flags: i32,
) -> Result<usize> {
    if name.is_empty() || name.len() > 255 {
        return Err(KernelError::RangeError);
    }
    if size > 2 * 1024 * 1024 {
        return Err(KernelError::RangeError);
    }
    Err(KernelError::NotSupported)
}

pub async fn sys_setxattr(
    path: TUA<c_char>,
    name: TUA<c_char>,
    value: UA,
    size: usize,
    flags: i32,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = current_task_shared();

    let node = VFS.resolve_path(path, VFS.root_inode(), &task).await?;
    let mut buf = [0; 1024];
    setxattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
        value,
        size,
        flags,
    )
}

pub async fn sys_lsetxattr(
    path: TUA<c_char>,
    name: TUA<c_char>,
    value: UA,
    size: usize,
    flags: i32,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = current_task_shared();

    let node = VFS
        .resolve_path_nofollow(path, VFS.root_inode(), &task)
        .await?;
    let mut buf = [0; 1024];
    setxattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
        value,
        size,
        flags,
    )
}

pub async fn sys_fsetxattr(
    fd: Fd,
    name: TUA<c_char>,
    value: UA,
    size: usize,
    flags: i32,
) -> Result<usize> {
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
    setxattr(
        node,
        UserCStr::from_ptr(name).copy_from_user(&mut buf).await?,
        value,
        size,
        flags,
    )
}
