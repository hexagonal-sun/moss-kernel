use libkernel::error::{KernelError, Result};

use crate::sched::syscall_ctx::ProcessCtx;
use core::convert::Infallible;

use super::Pgid;

/// Userspace `pid_t` type.
pub type PidT = i32;

pub fn sys_getpid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    Ok(ctx.shared().process.pid.local() as _)
}

pub fn sys_getppid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    Ok(ctx
        .shared()
        .process
        .parent
        .lock_save_irq()
        .as_ref()
        .and_then(|x| x.upgrade())
        .map(|x| x.pid.in_ns(&ctx.shared().pid_ns()))
        .unwrap_or(0) as _)
}

pub fn sys_getpgid(ctx: &ProcessCtx, pid: PidT) -> Result<usize> {
    let pgid = if pid == 0 {
        ctx.shared().process.pgid_ref.lock_save_irq().clone()
    } else if let Some(task) = crate::process::pid_namespace::find_task(ctx.shared(), pid as u32) {
        task.process.pgid_ref.lock_save_irq().clone()
    } else {
        return Err(KernelError::NoProcess);
    };

    Ok(pgid.in_ns(&ctx.shared().pid_ns()) as _)
}

pub fn sys_setpgid(ctx: &ProcessCtx, pid: PidT, pgid: Pgid) -> Result<usize> {
    let _pid_op = crate::process::pid_namespace::PID_OPS.lock_save_irq();
    if pid < 0 || pgid.0 > i32::MAX as u32 {
        return Err(KernelError::InvalidValue);
    }
    let ours = &ctx.shared().process;
    let target = if pid == 0 {
        ours.clone()
    } else {
        crate::process::pid_namespace::find_task(ctx.shared(), pid as u32)
            .ok_or(KernelError::NoProcess)
            .and_then(|t| {
                if t.tid.0 != t.process.tgid.0 {
                    Err(KernelError::InvalidValue)
                } else {
                    Ok(t.process.clone())
                }
            })?
    };
    if target.tgid != ours.tgid
        && !target
            .parent
            .lock_save_irq()
            .as_ref()
            .and_then(|p| p.upgrade())
            .is_some_and(|p| p.tgid == ours.tgid)
    {
        return Err(KernelError::NoProcess);
    }
    if target.tgid != ours.tgid && target.did_exec.load(core::sync::atomic::Ordering::Acquire) {
        return Err(libkernel::error::FsError::PermissionDenied.into());
    }
    let our_sid = *ours.sid.lock_save_irq();
    let target_sid = *target.sid.lock_save_irq();
    if target_sid != our_sid || target_sid.0 == target.tgid.0 {
        return Err(KernelError::NotPermitted);
    }
    let local = if pgid.0 == 0 {
        target.pid.in_ns(&ctx.shared().pid_ns())
    } else {
        pgid.0
    };
    let identity = if local == target.pid.in_ns(&ctx.shared().pid_ns()) {
        target.pid.clone()
    } else {
        let groups: alloc::vec::Vec<_> = super::TG_LIST
            .lock_save_irq()
            .values()
            .filter_map(|t| t.upgrade())
            .collect();
        groups
            .into_iter()
            .find_map(|p| {
                let identity = p.pgid_ref.lock_save_irq().clone();
                (identity.in_ns(&ctx.shared().pid_ns()) == local
                    && *p.sid.lock_save_irq() == our_sid)
                    .then_some(identity)
            })
            .ok_or(KernelError::NotPermitted)?
    };
    *target.pgid.lock_save_irq() = Pgid(identity.global.0);
    *target.pgid_ref.lock_save_irq() = identity;

    Ok(0)
}
