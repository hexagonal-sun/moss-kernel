use core::ffi::c_char;

use libkernel::{
    error::{KernelError, Result},
    fs::{FileType, path::Path},
    memory::address::TUA,
};

use crate::{
    fs::syscalls::at::{AtFlags, resolve_at_start_node, resolve_path_flags},
    memory::uaccess::cstr::UserCStr,
    process::{fd_table::Fd, inotify::notify_attrib},
    sched::syscall_ctx::ProcessCtx,
};

pub async fn sys_fchmodat(
    ctx: &ProcessCtx,
    dirfd: Fd,
    path: TUA<c_char>,
    mode: u16,
    flags: i32,
) -> Result<usize> {
    if flags & !(AtFlags::AT_EMPTY_PATH | AtFlags::AT_SYMLINK_NOFOLLOW).bits() != 0 {
        return Err(KernelError::InvalidValue);
    }
    let flags = AtFlags::from_bits_retain(flags);

    let mut buf = [0; 1024];

    let task = ctx.shared().clone();
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let start_node = resolve_at_start_node(ctx, dirfd, path, flags).await?;

    let node = resolve_path_flags(dirfd, path, start_node, &task, flags).await?;
    let mut attr = node.getattr().await?;
    if attr.file_type == FileType::Symlink {
        return Err(KernelError::OpNotSupported);
    }

    task.creds.lock_save_irq().chmod(&mut attr, mode)?;
    node.setattr(attr).await?;
    notify_attrib(node.id()).await;

    Ok(0)
}
