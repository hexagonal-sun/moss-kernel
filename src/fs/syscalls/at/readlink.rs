use crate::{
    fs::syscalls::at::{AtFlags, resolve_at_start_node, resolve_path_flags},
    memory::uaccess::{copy_to_user_slice, cstr::UserCStr},
    process::fd_table::Fd,
    sched::syscall_ctx::ProcessCtx,
};
use core::{cmp::min, ffi::c_char};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{FileType, path::Path},
    memory::address::{TUA, UA},
};

pub async fn sys_readlinkat(
    ctx: &ProcessCtx,
    dirfd: Fd,
    path: TUA<c_char>,
    buf: UA,
    size: usize,
) -> Result<usize> {
    if size == 0 {
        return Err(KernelError::InvalidValue);
    }
    let mut path_buf = [0; 1024];

    let task = ctx.shared().clone();
    let path = Path::new(
        UserCStr::from_ptr(path)
            .copy_from_user(&mut path_buf)
            .await?,
    );

    // Linux readlinkat accepts an empty pathname without a separate flags
    // argument, including an O_PATH | O_NOFOLLOW handle to the symlink.
    let flags = AtFlags::AT_EMPTY_PATH | AtFlags::AT_SYMLINK_NOFOLLOW;
    let start = resolve_at_start_node(ctx, dirfd, path, flags).await?;
    let inode = resolve_path_flags(dirfd, path, start, &task, flags).await?;
    let attr = inode.getattr().await?;

    if attr.file_type != FileType::Symlink {
        return Err(FsError::InvalidInput.into());
    }

    let target = inode.readlink().await?;
    let bytes = target.as_str().as_bytes();
    let len = min(bytes.len(), size);

    copy_to_user_slice(&bytes[..len], buf).await?;
    Ok(len)
}
