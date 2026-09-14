//! Namespace syscall entry points. Unsupported domains fail before any state
//! changes; user namespace creation detaches the complete CLONE_FS context.
use super::fd_table::Fd;
use super::{Task, clone::CloneFlags, user_namespace::UserNamespace};
use crate::{fs::VFS, sched::syscall_ctx::ProcessCtx};
use alloc::sync::Arc;
use libkernel::error::{KernelError, Result};
use libkernel::proc::caps::CapabilitiesFlags;

pub fn check_userns_create(task: &Task) -> Result<()> {
    if task.fs().root.lock_save_irq().0.id() != VFS.root_inode().id() {
        return Err(KernelError::NotPermitted);
    }
    Ok(())
}

pub fn sys_unshare(ctx: &ProcessCtx, flags: usize) -> Result<usize> {
    let supported = (CloneFlags::CLONE_FS | CloneFlags::CLONE_NEWUSER).bits() as usize;
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
    let fs = task.fs().duplicate();
    *task.fs.lock_save_irq() = fs;
    *task.creds.lock_save_irq() = creds;
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
        .downcast_ref::<crate::drivers::fs::nsfs::UserNsInode>()
        .ok_or(KernelError::InvalidValue)?;
    if kind != 0 && kind != CloneFlags::CLONE_NEWUSER.bits() {
        return Err(KernelError::InvalidValue);
    }
    let mut creds = task.creds.lock_save_irq().clone();
    if creds.user_ns() == inode.ns
        || task.process.tasks.lock_save_irq().len() != 1
        || Arc::strong_count(&task.fs.lock_save_irq()) != 1
    {
        return Err(KernelError::InvalidValue);
    }
    creds.check_capable_in(&inode.ns, CapabilitiesFlags::CAP_SYS_ADMIN)?;
    creds.enter_user_ns(inode.ns.clone());
    *task.creds.lock_save_irq() = creds;
    Ok(0)
}
