use alloc::boxed::Box;

use crate::drivers::block::get_block_device_by_descriptor;
use crate::fs::VFS;
use crate::memory::uaccess::cstr::UserCStr;
use crate::sched::syscall_ctx::ProcessCtx;
use bitflags::bitflags;
use core::ffi::c_char;
use libkernel::error::{FsError, KernelError, Result};
use libkernel::fs::{BlockDevice, FileType, path::Path};
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

fn mount_filesystem_type(name: &str) -> &str {
    match name {
        "proc" | "procfs" => "procfs",
        "devtmpfs" | "devfs" => "devfs",
        "cgroup2" | "cgroupfs" => "cgroupfs",
        "sysfs" => "sysfs",
        "tmpfs" => "tmpfs",
        "ext4" | "ext4fs" => "ext4fs",
        "fat" | "fat32" | "fat32fs" | "msdos" | "vfat" => "fat32fs",
        other => other,
    }
}

fn fallback_mount_filesystem_type(source: &str) -> Option<&str> {
    match source {
        "proc" | "procfs" => Some("procfs"),
        "devtmpfs" | "devfs" => Some("devfs"),
        "cgroup2" | "cgroupfs" => Some("cgroupfs"),
        "sysfs" => Some("sysfs"),
        "tmpfs" => Some("tmpfs"),
        _ => None,
    }
}

fn source_is_non_device(source: &str, fs_type: &str) -> bool {
    matches!(source, "" | "none")
        || matches!(
            (source, fs_type),
            ("proc", "procfs")
                | ("procfs", "procfs")
                | ("devtmpfs", "devfs")
                | ("devfs", "devfs")
                | ("cgroup2", "cgroupfs")
                | ("cgroupfs", "cgroupfs")
                | ("sysfs", "sysfs")
                | ("tmpfs", "tmpfs")
        )
}

async fn resolve_mount_block_device(
    ctx: &ProcessCtx,
    source: &str,
) -> Result<Box<dyn BlockDevice>> {
    let task = ctx.shared().clone();
    let cwd = task.cwd.lock_save_irq().0.clone();
    let inode = VFS.resolve_path(Path::new(source), cwd, &task).await?;

    match inode.getattr().await?.file_type {
        FileType::BlockDevice(device_id) => get_block_device_by_descriptor(device_id)
            .map(|(_, device)| device)
            .ok_or(FsError::NoDevice.into()),
        _ => Err(FsError::NoDevice.into()),
    }
}

pub async fn sys_mount(
    ctx: &ProcessCtx,
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
    let dev_name = if dev_name.is_null() {
        None
    } else {
        Some(
            UserCStr::from_ptr(dev_name)
                .copy_from_user(&mut buf)
                .await?,
        )
    };

    let mut buf = [0u8; 1024];
    let dir_name = UserCStr::from_ptr(dir_name)
        .copy_from_user(&mut buf)
        .await?;

    let mut buf = [0u8; 1024];
    let mount_type = if type_.is_null() {
        None
    } else {
        Some(UserCStr::from_ptr(type_).copy_from_user(&mut buf).await?)
    };

    let task = ctx.shared().clone();
    let cwd = task.cwd.lock_save_irq().0.clone();
    let mount_point = VFS.resolve_path(Path::new(dir_name), cwd, &task).await?;

    let fs_type = if let Some(mount_type) = mount_type {
        mount_filesystem_type(mount_type)
    } else if let Some(source) = dev_name {
        fallback_mount_filesystem_type(source).ok_or(KernelError::NotSupported)?
    } else {
        return Err(KernelError::NotSupported);
    };

    let blkdev = if let Some(source) = dev_name {
        if source_is_non_device(source, fs_type) {
            None
        } else {
            Some(resolve_mount_block_device(ctx, source).await?)
        }
    } else {
        None
    };

    VFS.mount(mount_point, fs_type, blkdev).await?;
    Ok(0)
}
