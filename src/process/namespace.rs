//! Namespace syscall entry points. Unsupported domains fail before any state
//! changes; user namespace creation detaches the complete CLONE_FS context.
use super::fd_table::Fd;
use super::{Task, clone::CloneFlags, user_namespace::UserNamespace};
use crate::sched::syscall_ctx::ProcessCtx;
use alloc::sync::Arc;
use libkernel::error::{KernelError, Result};
use libkernel::proc::caps::CapabilitiesFlags;

pub fn next_namespace_id() -> u64 {
    // ID 1 belongs to the initial user namespace. All namespace kinds share
    // the nsfs inode-number space.
    static NEXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(2);
    NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

pub fn check_userns_create(task: &Task) -> Result<()> {
    if *task.fs().root.lock_save_irq() != task.mount_ns().root() {
        return Err(KernelError::NotPermitted);
    }
    Ok(())
}

pub fn sys_unshare(ctx: &ProcessCtx, flags: usize) -> Result<usize> {
    let supported = (CloneFlags::CLONE_FS
        | CloneFlags::CLONE_NEWUSER
        | CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWPID)
        .bits() as usize;
    if flags & !supported != 0 {
        return Err(KernelError::InvalidValue);
    }
    if flags == 0 {
        return Ok(0);
    }
    let task = ctx.shared();
    let mut creds = task.creds.lock_save_irq().clone();
    if flags & CloneFlags::CLONE_NEWUSER.bits() as usize != 0 {
        if task.process.tasks.lock_save_irq().len() != 1 {
            return Err(KernelError::InvalidValue);
        }
        check_userns_create(task)?;
        let ns = UserNamespace::create(&creds)?;
        creds.enter_user_ns(ns);
    }
    let mut pid_ns = task.child_pid_ns();
    if flags & CloneFlags::CLONE_NEWPID.bits() as usize != 0 {
        if pid_ns.id != task.pid_ns().id || task.process.tasks.lock_save_irq().len() != 1 {
            return Err(KernelError::InvalidValue);
        }
        creds
            .caps()
            .check_capable(CapabilitiesFlags::CAP_SYS_ADMIN)?;
        pid_ns = super::pid_namespace::PidNamespace::create(pid_ns, creds.user_ns())?;
    }
    let fs = if flags
        & (CloneFlags::CLONE_FS | CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS).bits()
            as usize
        != 0
    {
        task.fs().duplicate()
    } else {
        task.fs()
    };
    let mut mount_ns = task.mount_ns();
    if flags & CloneFlags::CLONE_NEWNS.bits() as usize != 0 {
        creds
            .caps()
            .check_capable(CapabilitiesFlags::CAP_SYS_ADMIN)?;
        let (new_ns, map) = mount_ns.duplicate(creds.user_ns());
        fs.remap_mounts(&map);
        mount_ns = new_ns;
    }
    *task.fs.lock_save_irq() = fs;
    *task.creds.lock_save_irq() = creds;
    *task.mount_ns.lock_save_irq() = mount_ns;
    *task.pid_for_children.lock_save_irq() = pid_ns;
    Ok(0)
}

pub fn sys_setns(ctx: &ProcessCtx, fd: Fd, kind: u32) -> Result<usize> {
    let task = ctx.shared();
    let file = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;
    let inode = file.inode().ok_or(KernelError::InvalidValue)?;
    let inode = inode
        .as_any()
        .downcast_ref::<crate::drivers::fs::nsfs::NamespaceInode>()
        .ok_or(KernelError::InvalidValue)?;
    if kind != 0 && kind != inode.ns.kind() {
        return Err(KernelError::InvalidValue);
    }
    if !matches!(inode.ns, crate::drivers::fs::nsfs::Namespace::Pid(_))
        && Arc::strong_count(&task.fs.lock_save_irq()) != 1
    {
        return Err(KernelError::InvalidValue);
    }
    let mut creds = task.creds.lock_save_irq().clone();
    match &inode.ns {
        crate::drivers::fs::nsfs::Namespace::Pid(ns) => {
            creds.check_capable_in(&ns.owner, CapabilitiesFlags::CAP_SYS_ADMIN)?;
            creds
                .caps()
                .check_capable(CapabilitiesFlags::CAP_SYS_ADMIN)?;
            if !ns.within(&task.pid_ns()) {
                return Err(KernelError::InvalidValue);
            }
            *task.pid_for_children.lock_save_irq() = ns.clone();
        }
        crate::drivers::fs::nsfs::Namespace::User(ns) => {
            if creds.user_ns() == *ns || task.process.tasks.lock_save_irq().len() != 1 {
                return Err(KernelError::InvalidValue);
            }
            creds.check_capable_in(ns, CapabilitiesFlags::CAP_SYS_ADMIN)?;
            creds.enter_user_ns(ns.clone());
            *task.creds.lock_save_irq() = creds;
        }
        crate::drivers::fs::nsfs::Namespace::Mount(ns) => {
            creds.check_capable_in(&ns.owner, CapabilitiesFlags::CAP_SYS_ADMIN)?;
            creds.caps().check_capable(
                CapabilitiesFlags::CAP_SYS_ADMIN | CapabilitiesFlags::CAP_SYS_CHROOT,
            )?;
            let root = crate::fs::mount::MountNamespace::follow(ns.root());
            let fs = task.fs();
            *fs.root.lock_save_irq() = root.clone();
            *fs.cwd.lock_save_irq() = root;
            *task.mount_ns.lock_save_irq() = ns.clone();
        }
    }
    Ok(0)
}
