use alloc::{borrow::ToOwned, sync::Arc};

use crate::{
    fs::{VFS, syscalls::at::AtFlags},
    memory::uaccess::cstr::UserCStr,
    process::fd_table::{Fd, FdFlags},
    sched::syscall_ctx::ProcessCtx,
};
use core::ffi::c_char;
use libkernel::{
    error::Result,
    fs::{OpenFlags, attr::FilePermissions, path::Path, pathbuf::PathBuf},
    memory::address::TUA,
};

use super::resolve_at_start_node;

pub async fn sys_openat(
    ctx: &ProcessCtx,
    dirfd: Fd,
    path: TUA<c_char>,
    flags: u32,
    mode: u16,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let task = ctx.shared().clone();
    let flags = OpenFlags::from_bits_truncate(flags);
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let start_node = resolve_at_start_node(ctx, dirfd, path, AtFlags::empty()).await?;
    let mode = FilePermissions::from_bits_retain(mode);
    let display_path = if path.is_absolute() {
        path.to_owned()
    } else if dirfd.is_atcwd() {
        let mut full = task.cwd.lock_save_irq().1.clone();
        full.push(path);
        full
    } else {
        let base = task
            .fd_table
            .lock_save_irq()
            .get(dirfd)
            .and_then(|file| file.path().map(|p| p.to_owned()));
        if let Some(mut base) = base {
            base.push(path);
            base
        } else {
            PathBuf::from(path.as_str())
        }
    };

    let mut file = VFS.open(path, flags, start_node, mode, &task).await?;

    if let Some(inode) = file.inode()
        && let Some(of) = Arc::get_mut(&mut file)
    {
        of.update(inode, display_path);
    }

    let fd_flags = if flags.contains(OpenFlags::O_CLOEXEC) {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };
    let fd = task
        .fd_table
        .lock_save_irq()
        .insert_with_flags(file, fd_flags)?;

    Ok(fd.as_raw() as _)
}
