use super::{AtFlags, resolve_at_start_node};
use crate::{
    fs::syscalls::at::resolve_path_flags, memory::uaccess::cstr::UserCStr, process::fd_table::Fd,
    sched::current_task,
};
use core::ffi::c_char;
use libkernel::{
    error::Result,
    fs::{attr::AccessMode, path::Path},
    memory::address::TUA,
};

pub async fn sys_faccessat(dirfd: Fd, path: TUA<c_char>, mode: i32) -> Result<usize> {
    sys_faccessat2(dirfd, path, mode, 0).await
}

pub async fn sys_faccessat2(dirfd: Fd, path: TUA<c_char>, mode: i32, flags: i32) -> Result<usize> {
    let mut buf = [0; 1024];

    let task = current_task();
    let access_mode = AccessMode::from_bits_retain(mode);
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let start_node = resolve_at_start_node(dirfd, path).await?;
    let at_flags = AtFlags::from_bits_retain(flags);
    let node = resolve_path_flags(dirfd, path, start_node, task.clone(), at_flags).await?;

    // If mode is F_OK (value 0), the check is for the file's existence.
    // Reaching this point means we found the file, so we can return success.
    if mode == 0 {
        return Ok(0);
    }

    let attrs = node.getattr().await?;
    let creds = task.creds.lock_save_irq();

    // Determine which user and group IDs to use for the check. By default, use
    // the real UID and GID. If AT_EACCESS is set, use effective IDs.
    let (uid, gid) = if at_flags.contains(AtFlags::AT_EACCESS) {
        (creds.euid(), creds.egid())
    } else {
        (creds.uid(), creds.gid())
    };

    attrs.check_access(uid, gid, access_mode).map(|_| 0)
}
