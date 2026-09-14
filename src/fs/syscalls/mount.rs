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
    data: UA,
) -> Result<usize> {
    let task = ctx.shared();
    let creds = task.creds.lock_save_irq().clone();
    let ns = task.mount_ns();
    creds.check_capable_in(
        &ns.owner,
        libkernel::proc::caps::CapabilitiesFlags::CAP_SYS_ADMIN,
    )?;
    let flags = flags as u64 & !MountFlags::MS_SILENT.bits();
    let attrs = crate::fs::mount::attributes::SUPPORTED;
    let propagation = flags
        & (MountFlags::MS_SHARED
            | MountFlags::MS_SLAVE
            | MountFlags::MS_PRIVATE
            | MountFlags::MS_UNBINDABLE)
            .bits();
    let remount = flags & MountFlags::MS_REMOUNT.bits() != 0;
    let bind = flags & MountFlags::MS_BIND.bits() != 0;
    let recursive = flags & MountFlags::MS_REC.bits() != 0;
    if propagation != 0 {
        if propagation.count_ones() != 1 || flags & !(propagation | MountFlags::MS_REC.bits()) != 0
        {
            return Err(KernelError::InvalidValue);
        }
    } else {
        let allowed = if remount {
            MountFlags::MS_REMOUNT.bits() | MountFlags::MS_BIND.bits() | attrs
        } else if bind {
            MountFlags::MS_BIND.bits() | MountFlags::MS_REC.bits() | attrs
        } else {
            attrs
        };
        if flags & !allowed != 0 {
            return Err(KernelError::NotSupported);
        }
    }
    let mut dir_buf = [0u8; 1024];
    let dir = Path::new(
        UserCStr::from_ptr(dir_name)
            .copy_from_user(&mut dir_buf)
            .await?,
    );
    let cwd = task.fs().cwd.lock_save_irq().clone();
    let target = VFS.resolve_path(dir, cwd.clone(), task).await?;
    if !ns.contains(&target) {
        return Err(KernelError::InvalidValue);
    }
    if propagation != 0 || remount {
        let target = crate::fs::mount::MountNamespace::follow(target);
        if propagation != 0 {
            ns.change_propagation(&target, propagation, recursive)?;
        } else {
            if !data.is_null() {
                let mut options = [0; 4096];
                if !UserCStr::from_ptr(TUA::from_value(data.value()))
                    .copy_from_user(&mut options)
                    .await?
                    .is_empty()
                {
                    return Err(KernelError::NotSupported);
                }
            }
            ns.remount(&target, flags & attrs, bind, &creds)?;
        }
        return Ok(0);
    }
    let mut source_buf = [0u8; 1024];
    let source = if dev_name.is_null() {
        None
    } else {
        Some(
            UserCStr::from_ptr(dev_name)
                .copy_from_user(&mut source_buf)
                .await?,
        )
    };
    if bind {
        let source = source.ok_or(KernelError::Fault)?;
        let source = VFS.resolve_path(Path::new(source), cwd, task).await?;
        VFS.bind(&ns, source, target, recursive).await?;
        return Ok(0);
    }
    let mut type_buf = [0u8; 128];
    let fs_type = if type_.is_null() {
        None
    } else {
        Some(
            UserCStr::from_ptr(type_)
                .copy_from_user(&mut type_buf)
                .await?,
        )
    };
    let fs_name = match fs_type.or(source).ok_or(KernelError::NotSupported)? {
        "proc" => "procfs",
        "devtmpfs" => "devfs",
        "cgroup2" => "cgroupfs",
        s => s,
    };
    // Global pseudo-filesystems do not yet have userns-safe superblocks.
    if creds.user_ns() != crate::process::user_namespace::UserNamespace::initial()
        && fs_name != "tmpfs"
        && fs_name != "procfs"
    {
        return Err(KernelError::NotPermitted);
    }
    if fs_name == "procfs" {
        creds.check_capable_in(
            &task.pid_ns().owner,
            libkernel::proc::caps::CapabilitiesFlags::CAP_SYS_ADMIN,
        )?;
    }
    if !data.is_null() {
        let mut options = [0u8; 4096];
        if !UserCStr::from_ptr(TUA::from_value(data.value()))
            .copy_from_user(&mut options)
            .await?
            .is_empty()
        {
            return Err(KernelError::NotSupported);
        }
    }
    VFS.mount_flags(&ns, target, fs_name, None, Some(&creds), flags & attrs)
        .await?;
    Ok(0)
}

pub async fn sys_umount2(ctx: &ProcessCtx, target: TUA<c_char>, flags: u32) -> Result<usize> {
    if flags & !15 != 0 {
        return Err(KernelError::InvalidValue);
    }
    if flags & (1 | 4) != 0 {
        return Err(KernelError::NotSupported);
    }
    let task = ctx.shared();
    let ns = task.mount_ns();
    task.creds.lock_save_irq().check_capable_in(
        &ns.owner,
        libkernel::proc::caps::CapabilitiesFlags::CAP_SYS_ADMIN,
    )?;
    let mut buf = [0; 1024];
    let name = Path::new(UserCStr::from_ptr(target).copy_from_user(&mut buf).await?);
    let cwd = task.fs().cwd.lock_save_irq().clone();
    let target = if flags & 8 != 0 {
        VFS.resolve_path_nofollow(name, cwd, task).await?
    } else {
        VFS.resolve_path(name, cwd, task).await?
    };
    let target = crate::fs::mount::MountNamespace::follow(target);
    ns.unmount(&target, flags & 2 != 0)?;
    Ok(0)
}
