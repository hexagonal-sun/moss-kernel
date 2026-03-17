use crate::fs::VFS;
use crate::fs::syscalls::at::{AtFlags, resolve_at_start_node};
use crate::memory::uaccess::cstr::UserCStr;
use crate::process::fd_table::Fd;
use crate::sched::syscall_ctx::ProcessCtx;
use core::ffi::c_char;
use libkernel::fs::attr::FilePermissions;
use libkernel::fs::path::Path;
use libkernel::memory::address::TUA;

pub async fn sys_mkdirat(
    ctx: &ProcessCtx,
    dirfd: Fd,
    path: TUA<c_char>,
    mode: u16,
) -> libkernel::error::Result<usize> {
    let mut buf = [0; 1024];

    let task = ctx.shared().clone();
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let start_node = resolve_at_start_node(ctx, dirfd, path, AtFlags::empty()).await?;
    let mode = FilePermissions::from_bits_retain(mode);

    VFS.mkdir(path, start_node, mode, &task).await?;
    Ok(0)
}
