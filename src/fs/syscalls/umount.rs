use crate::{fs::VFS, memory::uaccess::cstr::UserCStr, sched::syscall_ctx::ProcessCtx};
use bitflags::bitflags;
use core::ffi::c_char;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::path::Path,
    memory::address::TUA,
    proc::caps::CapabilitiesFlags,
};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct UmountFlags: u32 {
        const MNT_FORCE = 0x1;
        const MNT_DETACH = 0x2;
        const MNT_EXPIRE = 0x4;
        const UMOUNT_NOFOLLOW = 0x8;
    }
}

pub async fn sys_umount2(ctx: &ProcessCtx, target: TUA<c_char>, flags: i64) -> Result<usize> {
    let flags = u32::try_from(flags)
        .ok()
        .and_then(UmountFlags::from_bits)
        .ok_or(KernelError::InvalidValue)?;

    if flags.contains(UmountFlags::MNT_EXPIRE)
        && flags.intersects(UmountFlags::MNT_FORCE | UmountFlags::MNT_DETACH)
    {
        return Err(KernelError::InvalidValue);
    }

    if flags.contains(UmountFlags::MNT_EXPIRE) {
        // TODO: Implement two-phase expiry semantics.
        return Err(KernelError::TryAgain);
    }

    let task = ctx.shared().clone();
    task.creds
        .lock_save_irq()
        .caps()
        .check_capable(CapabilitiesFlags::CAP_SYS_ADMIN)?;

    let mut buf = [0u8; 1024];
    let target = UserCStr::from_ptr(target).copy_from_user(&mut buf).await?;
    if target.is_empty() {
        return Err(FsError::NotFound.into());
    }

    let cwd = task.cwd.lock_save_irq().0.clone();
    let target_path = Path::new(target);
    let target = if flags.contains(UmountFlags::UMOUNT_NOFOLLOW) {
        VFS.resolve_path_nofollow(target_path, cwd, &task).await?
    } else {
        VFS.resolve_path(target_path, cwd, &task).await?
    };

    VFS.umount(target, flags.contains(UmountFlags::MNT_DETACH))
        .await?;

    Ok(0)
}
