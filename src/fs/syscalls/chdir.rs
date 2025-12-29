use crate::{
    fs::VFS,
    memory::uaccess::{copy_to_user_slice, cstr::UserCStr},
    process::fd_table::Fd,
    sched::current_task,
};
use alloc::{borrow::ToOwned, ffi::CString, string::ToString};
use core::{ffi::c_char, str::FromStr};
use libkernel::{
    error::{KernelError, Result},
    fs::path::Path,
    memory::address::{TUA, UA},
    proc::caps::CapabilitiesFlags,
};

pub async fn sys_getcwd(buf: UA, len: usize) -> Result<usize> {
    let task = current_task();
    let path = task.cwd.lock_save_irq().1.as_str().to_string();
    let cstr = CString::from_str(&path).map_err(|_| KernelError::InvalidValue)?;
    let slice = cstr.as_bytes_with_nul();

    if slice.len() > len {
        return Err(KernelError::TooLarge);
    }

    copy_to_user_slice(slice, buf).await?;

    Ok(buf.value())
}

pub async fn sys_chdir(path: TUA<c_char>) -> Result<usize> {
    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let task = current_task();
    let current_path = task.cwd.lock_save_irq().0.clone();
    let new_path = task.cwd.lock_save_irq().1.join(path);

    let node = VFS.resolve_path(path, current_path, task.clone()).await?;

    *task.cwd.lock_save_irq() = (node, new_path);

    Ok(0)
}

pub async fn sys_chroot(path: TUA<c_char>) -> Result<usize> {
    let task = current_task();
    task.creds
        .lock_save_irq()
        .caps()
        .check_capable(CapabilitiesFlags::CAP_SYS_CHROOT)?;

    let mut buf = [0; 1024];

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let current_path = task.root.lock_save_irq().0.clone();
    let new_path = task.root.lock_save_irq().1.join(path);

    let node = VFS.resolve_path(path, current_path, task.clone()).await?;

    *task.root.lock_save_irq() = (node, new_path);

    Ok(0)
}

pub async fn sys_fchdir(fd: Fd) -> Result<usize> {
    let task = current_task();
    let file = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    *task.cwd.lock_save_irq() = (
        file.inode().ok_or(KernelError::BadFd)?,
        file.path().ok_or(KernelError::BadFd)?.to_owned(),
    );

    Ok(0)
}
