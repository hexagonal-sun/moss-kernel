use crate::fs::VFS;
use crate::memory::uaccess::cstr::UserCStr;
use crate::sched::current::current_task_shared;
use core::ffi::c_char;
use libkernel::error::Result;
use libkernel::fs::path::Path;
use libkernel::memory::address::{TUA, UA};

pub async fn sys_mount(
    dev_name: TUA<c_char>,
    dir_name: TUA<c_char>,
    type_: TUA<c_char>,
    _flags: i64,
    _data: UA,
) -> Result<usize> {
    let mut buf = [0u8; 1024];
    let dev_name = UserCStr::from_ptr(dev_name)
        .copy_from_user(&mut buf)
        .await?;
    let mut buf = [0u8; 1024];
    let dir_name = UserCStr::from_ptr(dir_name)
        .copy_from_user(&mut buf)
        .await?;
    let mount_point = VFS
        .resolve_path(
            Path::new(dir_name),
            VFS.root_inode(),
            &current_task_shared(),
        )
        .await?;
    let mut buf = [0u8; 1024];
    let _type = UserCStr::from_ptr(type_).copy_from_user(&mut buf).await?;
    let dev_name = match dev_name {
        "proc" => "procfs",
        "devtmpfs" => "devfs",
        s => s,
    };
    VFS.mount(mount_point, dev_name, None).await?;
    Ok(0)
}
