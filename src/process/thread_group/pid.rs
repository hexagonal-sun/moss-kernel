use libkernel::error::{KernelError, Result};

use crate::sched::syscall_ctx::ProcessCtx;
use core::convert::Infallible;

use super::{Pgid, Tgid, ThreadGroup};

/// Userspace `pid_t` type.
pub type PidT = i32;

pub fn sys_getpid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    Ok(ctx.shared().process.tgid.value() as _)
}

pub fn sys_getppid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    Ok(ctx
        .shared()
        .process
        .parent
        .lock_save_irq()
        .as_ref()
        .and_then(|x| x.upgrade())
        .map(|x| x.tgid.value())
        .unwrap_or(0) as _)
}

pub fn sys_getpgid(ctx: &ProcessCtx, pid: PidT) -> Result<usize> {
    let pgid = if pid == 0 {
        *ctx.shared().process.pgid.lock_save_irq()
    } else if let Some(tg) = ThreadGroup::get(Tgid::from_pid_t(pid)) {
        *tg.pgid.lock_save_irq()
    } else {
        return Err(KernelError::NoProcess);
    };

    Ok(pgid.value() as _)
}

pub fn sys_setpgid(ctx: &ProcessCtx, pid: PidT, pgid: Pgid) -> Result<usize> {
    if pid == 0 {
        *ctx.shared().process.pgid.lock_save_irq() = pgid;
    } else if let Some(tg) = ThreadGroup::get(Tgid::from_pid_t(pid)) {
        *tg.pgid.lock_save_irq() = pgid;
    } else {
        return Err(KernelError::NoProcess);
    };

    Ok(0)
}
