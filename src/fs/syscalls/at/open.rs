use crate::{
    fs::{VFS, syscalls::at::AtFlags},
    memory::uaccess::cstr::UserCStr,
    process::fd_table::Fd,
    sched::current_task,
};
use core::ffi::c_char;
use libkernel::{
    error::Result,
    fs::{OpenFlags, attr::FilePermissions, path::Path},
    memory::address::TUA,
};

use super::resolve_at_start_node;

pub async fn sys_openat(dirfd: Fd, path: TUA<c_char>, flags: u32, mode: u16) -> Result<usize> {
    let mut buf = [0; 1024];

    let task = current_task();
    let flags = OpenFlags::from_bits_truncate(flags);
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let start_node = resolve_at_start_node(dirfd, path, AtFlags::empty()).await?;
    let mode = FilePermissions::from_bits_retain(mode);

    let file = VFS
        .open(path, flags, start_node, mode, task.clone())
        .await?;

    let fd = task.fd_table.lock_save_irq().insert(file)?;

    Ok(fd.as_raw() as _)
}
