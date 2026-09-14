use crate::{
    fs::VFS,
    memory::uaccess::{copy_to_user_slice, cstr::UserCStr},
    process::fd_table::Fd,
    sched::syscall_ctx::ProcessCtx,
};
use alloc::ffi::CString;
use core::{ffi::c_char, str::FromStr};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{FileType, attr::AccessMode, path::Path},
    memory::address::{TUA, UA},
    proc::caps::CapabilitiesFlags,
};

pub async fn sys_getcwd(ctx: &ProcessCtx, buf: UA, len: usize) -> Result<usize> {
    let task = ctx.shared().clone();
    let fs = task.fs();
    let cwd = fs.cwd.lock_save_irq().clone();
    let root = fs.root.lock_save_irq().clone();
    let path = cwd.relative_to(&root).ok_or(FsError::NotFound)?;
    let cstr = CString::from_str(path.as_str()).map_err(|_| KernelError::InvalidValue)?;
    let slice = cstr.as_bytes_with_nul();

    if slice.len() > len {
        return Err(KernelError::RangeError);
    }

    copy_to_user_slice(slice, buf).await?;

    // Linux returns the length of the copied pathname, including the trailing
    // NUL. libc uses this value to validate the result before returning `buf`.
    Ok(slice.len())
}

pub async fn sys_chdir(ctx: &ProcessCtx, path: TUA<c_char>) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = ctx.shared().clone();
    let current_path = task.fs().cwd.lock_save_irq().clone();

    let node = VFS.resolve_path(path, current_path, &task).await?;
    let attr = node.getattr().await?;
    if attr.file_type != FileType::Directory {
        return Err(FsError::NotADirectory.into());
    }
    task.creds
        .lock_save_irq()
        .check_file_access(&attr, AccessMode::X_OK)?;

    *task.fs().cwd.lock_save_irq() = node;

    Ok(0)
}

pub async fn sys_chroot(ctx: &ProcessCtx, path: TUA<c_char>) -> Result<usize> {
    let task = ctx.shared().clone();
    task.creds
        .lock_save_irq()
        .caps()
        .check_capable(CapabilitiesFlags::CAP_SYS_CHROOT)?;

    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let current_path = task.fs().cwd.lock_save_irq().clone();

    let node = VFS.resolve_path(path, current_path, &task).await?;
    let attr = node.getattr().await?;
    if attr.file_type != FileType::Directory {
        return Err(FsError::NotADirectory.into());
    }
    task.creds
        .lock_save_irq()
        .check_file_access(&attr, AccessMode::X_OK)?;

    *task.fs().root.lock_save_irq() = node;

    Ok(0)
}

pub async fn sys_fchdir(ctx: &ProcessCtx, fd: Fd) -> Result<usize> {
    let task = ctx.shared().clone();
    let file = task
        .fd_table
        .lock_save_irq()
        .get_raw(fd)
        .ok_or(KernelError::BadFd)?;

    let inode = file.vfs_path().ok_or(KernelError::BadFd)?;
    let attr = inode.getattr().await?;
    if attr.file_type != FileType::Directory {
        return Err(FsError::NotADirectory.into());
    }
    task.creds
        .lock_save_irq()
        .check_file_access(&attr, AccessMode::X_OK)?;

    *task.fs().cwd.lock_save_irq() = inode;

    Ok(0)
}
