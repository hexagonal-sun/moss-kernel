use crate::fs::VFS;
use crate::memory::uaccess::cstr::UserCStr;
use crate::sched::current::current_task_shared;
use bitflags::bitflags;
use core::ffi::c_char;
use libkernel::error::{KernelError, Result};
use libkernel::fs::path::Path;
use libkernel::memory::address::{TUA, UA};

bitflags! {
    #[derive(Debug)]
    pub struct MountFlags: u64 {
        const MS_RDONLY = 1;
        const MS_NOSUID = 2;
        const MS_NODEV = 4;
        const MS_NOEXEC = 8;
        const MS_SYNCHRONOUS = 16;
        const MS_REMOUNT = 32;
        const MS_MANDLOCK = 64;
        const MS_DIRSYNC = 128;
        const NOSYMFOLLOW = 256;
        const MS_NOATIME = 1024;
        const MS_NODIRATIME = 2048;
        const MS_BIND = 4096;
        const MS_MOVE = 8192;
        const MS_REC = 16384;
        const MS_VERBOSE = 32768;
        const MS_SILENT = 65536;
        const MS_POSIXACL = 1 << 16;
        const MS_UNBINDABLE	= 1 << 17;
        const MS_PRIVATE = 1 << 18;
        const MS_SLAVE = 1 << 19;
        const MS_SHARED	= 1 << 20;
        const MS_RELATIME = 1 << 21;
        const MS_KERNMOUNT = 1 << 22;
        const MS_I_VERSION = 1 << 23;
        const MS_STRICTATIME = 1 << 24;
        const MS_LAZYTIME = 1 << 25;
        const MS_SUBMOUNT = 1 << 26;
        const MS_NOREMOTELOCK = 1 << 27;
        const MS_NOSEC = 1 << 28;
        const MS_BORN = 1 << 29;
        const MS_ACTIVE	= 1 << 30;
        const MS_NOUSER	= 1 << 31;
    }
}

pub async fn sys_mount(
    dev_name: TUA<c_char>,
    dir_name: TUA<c_char>,
    type_: TUA<c_char>,
    flags: i64,
    _data: UA,
) -> Result<usize> {
    let flags = MountFlags::from_bits_truncate(flags as u64);
    if flags.contains(MountFlags::MS_REC) {
        // TODO: Handle later
        return Ok(0);
    }
    let mut buf = [0u8; 1024];
    let dev_name = UserCStr::from_ptr(dev_name)
        .copy_from_user(&mut buf)
        .await
        .ok();
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
    let _type = UserCStr::from_ptr(type_)
        .copy_from_user(&mut buf)
        .await
        .ok();
    if let Some(dev_name) = dev_name {
        let dev_name = match dev_name {
            "proc" => "procfs",
            "devtmpfs" => "devfs",
            "cgroup2" => "cgroupfs",
            s => s,
        };
        VFS.mount(mount_point, dev_name, None).await?;
        Ok(0)
    } else {
        Err(KernelError::NotSupported)
    }
}
