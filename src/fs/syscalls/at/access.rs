use super::{AtFlags, resolve_at_start_node};
use crate::{
    fs::syscalls::at::resolve_path_flags, memory::uaccess::cstr::UserCStr, process::fd_table::Fd,
    sched::syscall_ctx::ProcessCtx,
};
use core::ffi::c_char;
use libkernel::{
    error::{KernelError, Result},
    fs::{attr::AccessMode, path::Path},
    memory::address::TUA,
};

pub async fn sys_faccessat(
    ctx: &ProcessCtx,
    dirfd: Fd,
    path: TUA<c_char>,
    mode: i32,
) -> Result<usize> {
    sys_faccessat2(ctx, dirfd, path, mode, 0).await
}

pub async fn sys_faccessat2(
    ctx: &ProcessCtx,
    dirfd: Fd,
    path: TUA<c_char>,
    mode: i32,
    flags: i32,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let task = ctx.shared().clone();
    let access_mode = AccessMode::from_bits(mode).ok_or(KernelError::InvalidValue)?;
    let allowed = AtFlags::AT_EACCESS | AtFlags::AT_SYMLINK_NOFOLLOW | AtFlags::AT_EMPTY_PATH;
    if flags & !allowed.bits() != 0 {
        return Err(KernelError::InvalidValue);
    }
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let at_flags = AtFlags::from_bits_retain(flags);
    let start_node = resolve_at_start_node(ctx, dirfd, path, at_flags).await?;
    let creds = task
        .creds
        .lock_save_irq()
        .for_access(at_flags.contains(AtFlags::AT_EACCESS));
    let node = if path.as_str().is_empty() {
        resolve_path_flags(dirfd, path, start_node, &task, at_flags).await?
    } else {
        crate::fs::VFS
            .resolve_with_credentials(
                path,
                start_node,
                &task,
                !at_flags.contains(AtFlags::AT_SYMLINK_NOFOLLOW),
                &creds,
            )
            .await?
    };

    // If mode is F_OK (value 0), the check is for the file's existence.
    // Reaching this point means we found the file, so we can return success.
    if mode == 0 {
        return Ok(0);
    }
    let kind = node.getattr().await?.file_type;
    if access_mode.contains(AccessMode::X_OK) && kind == libkernel::fs::FileType::File {
        node.check_exec_mount()?;
    }
    if access_mode.contains(AccessMode::W_OK)
        && !matches!(kind, libkernel::fs::FileType::CharDevice(_))
        && node.mount_flags() & crate::fs::mount::attributes::RDONLY != 0
    {
        return Err(KernelError::ReadOnly);
    }

    creds
        .check_inode_access(node.as_ref(), access_mode)
        .await
        .map(|_| 0)
}
