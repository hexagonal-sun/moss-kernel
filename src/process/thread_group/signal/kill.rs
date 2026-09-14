use crate::{
    process::{
        Tid,
        thread_group::{Pgid, Tgid, ThreadGroup, pid::PidT},
    },
    sched::syscall_ctx::ProcessCtx,
};

use super::{SigId, uaccess::UserSigId};
use crate::process::thread_group::TG_LIST;
use libkernel::error::{KernelError, Result};

fn signal_group(ctx: &ProcessCtx, target: &ThreadGroup, signal: Option<SigId>) -> Result<()> {
    let task = target
        .tasks
        .lock_save_irq()
        .values()
        .find_map(|work| work.upgrade())
        .ok_or(KernelError::NoProcess)?;
    crate::process::access::signal_may_access(ctx.shared(), &task, signal)?;
    if let Some(signal) = signal {
        target.deliver_signal(signal);
    }
    Ok(())
}

pub fn sys_kill(ctx: &ProcessCtx, pid: PidT, signal: UserSigId) -> Result<usize> {
    let signal = signal.optional()?;
    if pid > 0 {
        let target = ThreadGroup::get(Tgid(pid as u32)).ok_or(KernelError::NoProcess)?;
        signal_group(ctx, &target, signal)?;
        return Ok(0);
    }
    let our_pgid = *ctx.shared().process.pgid.lock_save_irq();
    // Do not keep the global list locked while taking per-task/credential locks.
    let groups: alloc::vec::Vec<_> = TG_LIST
        .lock_save_irq()
        .values()
        .filter_map(|tg| tg.upgrade())
        .collect();
    let mut result = Err(KernelError::NoProcess);
    let mut success = false;
    for tg in groups {
        let selected = if pid == -1 {
            tg.tgid.value() > 1 && tg.tgid != ctx.shared().process.tgid
        } else {
            let pgid = if pid == 0 {
                our_pgid
            } else {
                Pgid(pid.unsigned_abs())
            };
            *tg.pgid.lock_save_irq() == pgid
        };
        if selected {
            match signal_group(ctx, &tg, signal) {
                Ok(()) => success = true,
                Err(KernelError::NotPermitted) => result = Err(KernelError::NotPermitted),
                Err(_) => (),
            }
        }
    }
    if success { Ok(0) } else { result }
}

pub fn sys_tkill(ctx: &ProcessCtx, tid: PidT, signal: UserSigId) -> Result<usize> {
    if tid <= 0 {
        return Err(KernelError::InvalidValue);
    }
    let signal = signal.optional()?;
    let target = crate::process::find_task_by_tid(Tid(tid as u32)).ok_or(KernelError::NoProcess)?;
    crate::process::access::signal_may_access(ctx.shared(), &target, signal)?;
    if let Some(signal) = signal {
        target.raise_task_signal(signal);
    }
    Ok(0)
}

pub fn sys_tgkill(ctx: &ProcessCtx, tgid: PidT, tid: PidT, signal: UserSigId) -> Result<usize> {
    if tgid <= 0 || tid <= 0 {
        return Err(KernelError::InvalidValue);
    }
    let signal = signal.optional()?;
    let target = crate::process::find_task_by_tid(Tid(tid as u32)).ok_or(KernelError::NoProcess)?;
    if target.process.tgid.value() != tgid as u32 {
        return Err(KernelError::NoProcess);
    }
    crate::process::access::signal_may_access(ctx.shared(), &target, signal)?;
    if let Some(signal) = signal {
        target.raise_task_signal(signal);
    }
    Ok(0)
}

pub fn send_signal_to_pg(pgid: Pgid, signal: SigId) {
    for tg_weak in TG_LIST.lock_save_irq().values() {
        if let Some(tg) = tg_weak.upgrade()
            && *tg.pgid.lock_save_irq() == pgid
        {
            tg.deliver_signal(signal);
        }
    }
}
