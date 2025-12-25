use core::ffi::c_char;

use libkernel::{
    error::{KernelError, Result},
    fs::path::Path,
    memory::address::TUA,
};

use crate::{
    clock::{realtime::date, timespec::TimeSpec},
    current_task,
    fs::syscalls::at::{AtFlags, resolve_at_start_node, resolve_path_flags},
    memory::uaccess::{copy_from_user, cstr::UserCStr},
    process::fd_table::Fd,
};

pub async fn sys_utimensat(
    dirfd: Fd,
    path: TUA<c_char>,
    times: TUA<[TimeSpec; 2]>,
    flags: i32,
) -> Result<usize> {
    let task = current_task();

    // linux specifically uses NULL path to indicate futimens, see utimensat(2)
    let node = if path.is_null() {
        task.fd_table
            .lock_save_irq()
            .get(dirfd)
            .ok_or(KernelError::BadFd)?
            .inode()
            .ok_or(KernelError::BadFd)?
    } else {
        let mut buf = [0; 1024];

        let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
        let start_node = resolve_at_start_node(dirfd, path).await?;
        let flags = AtFlags::from_bits_retain(flags);

        resolve_path_flags(dirfd, path, start_node, task, flags).await?
    };

    let mut attr = node.getattr().await?;

    if times.is_null() {
        attr.atime = date();
        attr.mtime = date();
        attr.ctime = date();
    } else {
        let times = copy_from_user(times).await?;
        attr.atime = times[0].into();
        attr.mtime = times[1].into();
        attr.ctime = date();
    }

    node.setattr(attr).await?;

    Ok(0)
}
