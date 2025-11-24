use crate::{
    fs::VFS,
    memory::uaccess::{copy_to_user_slice, cstr::UserCStr},
    sched::current_task,
};
use alloc::{ffi::CString, string::ToString};
use core::{ffi::c_char, str::FromStr};
use libkernel::{
    error::{KernelError, Result},
    fs::path::Path,
    memory::address::{TUA, UA},
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

    let node = VFS.resolve_path(path, current_path).await?;

    *task.cwd.lock_save_irq() = (node, new_path);

    Ok(0)
}
