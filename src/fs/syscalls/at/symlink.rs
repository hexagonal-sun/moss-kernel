use core::ffi::c_char;

use libkernel::{error::Result, fs::path::Path, memory::address::TUA};

use crate::{
    fs::{
        VFS,
        syscalls::at::{AtFlags, resolve_at_start_node},
    },
    memory::uaccess::cstr::UserCStr,
    process::fd_table::Fd,
    sched::current_task,
};

pub async fn sys_symlinkat(
    old_name: TUA<c_char>,
    new_dirfd: Fd,
    new_name: TUA<c_char>,
) -> Result<usize> {
    let mut buf = [0; 1024];
    let mut buf2 = [0; 1024];

    let task = current_task();
    let source = Path::new(
        UserCStr::from_ptr(old_name)
            .copy_from_user(&mut buf)
            .await?,
    );
    let target = Path::new(
        UserCStr::from_ptr(new_name)
            .copy_from_user(&mut buf2)
            .await?,
    );
    let start_node = resolve_at_start_node(new_dirfd, target, AtFlags::empty()).await?;

    VFS.symlink(source, target, start_node, task).await?;

    Ok(0)
}
