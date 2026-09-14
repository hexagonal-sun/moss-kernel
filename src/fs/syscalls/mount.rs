use crate::fs::VFS;
use crate::memory::uaccess::cstr::UserCStr;
use crate::sched::syscall_ctx::ProcessCtx;
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
        const MS_SILENT = 32768;
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
    ctx: &ProcessCtx,
    dev_name: TUA<c_char>,
    dir_name: TUA<c_char>,
    type_: TUA<c_char>,
    flags: i64,
    _data: UA,
) -> Result<usize> {
    ctx.shared().creds.lock_save_irq().check_capable_in(
        &crate::process::user_namespace::UserNamespace::initial(),
        libkernel::proc::caps::CapabilitiesFlags::CAP_SYS_ADMIN,
    )?;
    // Mount options are not enforced by the global mount table yet. In
    // particular, never silently accept security flags or propagation changes.
    let harmless = MountFlags::MS_SILENT.bits();
    if flags as u64 & !harmless != 0 {
        return Err(KernelError::NotSupported);
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
    let cwd = ctx.shared().fs().cwd.lock_save_irq().0.clone();
    let mount_point = VFS
        .resolve_path(Path::new(dir_name), cwd, ctx.shared())
        .await?;
    let mut buf = [0u8; 1024];
    let fs_type = if type_.is_null() {
        None
    } else {
        Some(UserCStr::from_ptr(type_).copy_from_user(&mut buf).await?)
    };

    let fs_name = fs_type.or(dev_name).ok_or(KernelError::NotSupported)?;
    let fs_name = match fs_name {
        "proc" => "procfs",
        "devtmpfs" => "devfs",
        "sysfs" => "sysfs",
        "cgroup2" => "cgroupfs",
        s => s,
    };

    VFS.mount(mount_point, fs_name, None).await?;
    Ok(0)
}
