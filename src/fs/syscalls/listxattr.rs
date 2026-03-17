use crate::fs::VFS;
use crate::memory::uaccess::copy_to_user_slice;
use crate::memory::uaccess::cstr::UserCStr;
use crate::process::fd_table::Fd;
use crate::sched::syscall_ctx::ProcessCtx;
use alloc::sync::Arc;
use libkernel::error::{KernelError, Result};
use libkernel::fs::Inode;
use libkernel::fs::path::Path;
use libkernel::memory::address::{TUA, UA};

async fn listxattr(node: Arc<dyn Inode>, ua: UA, size: usize) -> Result<usize> {
    let list = node.listxattr().await?;
    // Join with \0
    let list = list.join("\0");
    let list_bytes = list.as_bytes();
    if size < list_bytes.len() {
        Err(KernelError::RangeError)
    } else {
        copy_to_user_slice(list_bytes, ua).await?;
        Ok(list_bytes.len())
    }
}

pub async fn sys_listxattr(
    ctx: &ProcessCtx,
    path: TUA<core::ffi::c_char>,
    list: UA,
    size: usize,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = ctx.shared().clone();

    let node = VFS.resolve_path(path, VFS.root_inode(), &task).await?;
    listxattr(node, list, size).await
}

pub async fn sys_llistxattr(
    ctx: &ProcessCtx,
    path: TUA<core::ffi::c_char>,
    list: UA,
    size: usize,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = ctx.shared().clone();

    let node = VFS
        .resolve_path_nofollow(path, VFS.root_inode(), &task)
        .await?;
    listxattr(node, list, size).await
}

pub async fn sys_flistxattr(ctx: &ProcessCtx, fd: Fd, list: UA, size: usize) -> Result<usize> {
    let node = {
        let task = ctx.shared().clone();
        let file = task
            .fd_table
            .lock_save_irq()
            .get(fd)
            .ok_or(KernelError::BadFd)?;

        file.inode().ok_or(KernelError::BadFd)?
    };
    listxattr(node, list, size).await
}
