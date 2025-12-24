use core::ffi::c_char;

use libkernel::{error::Result, fs::path::Path, memory::address::TUA};

use crate::{
    fs::{VFS, syscalls::at::resolve_at_start_node},
    memory::uaccess::cstr::UserCStr,
    process::fd_table::Fd,
    sched::current_task,
};

pub async fn sys_linkat(
    old_dirfd: Fd,
    old_path: TUA<c_char>,
    new_dirfd: Fd,
    new_path: TUA<c_char>,
) -> Result<usize> {
    let mut buf = [0; 1024];
    let mut buf2 = [0; 1024];

    let task = current_task();
    let old_path = Path::new(
        UserCStr::from_ptr(old_path)
            .copy_from_user(&mut buf)
            .await?,
    );
    let new_path = Path::new(
        UserCStr::from_ptr(new_path)
            .copy_from_user(&mut buf2)
            .await?,
    );
    let old_start_node = resolve_at_start_node(old_dirfd, old_path).await?;
    let new_start_node = resolve_at_start_node(new_dirfd, new_path).await?;

    VFS.link(old_path, new_path, old_start_node, new_start_node, task)
        .await?;

    Ok(0)
}
