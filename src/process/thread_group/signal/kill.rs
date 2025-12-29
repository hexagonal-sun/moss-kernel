use crate::{
    process::{
        Tid,
        thread_group::{Pgid, Tgid, ThreadGroup, pid::PidT},
    },
    sched::current_task,
};

use super::{SigId, uaccess::UserSigId};
use crate::process::thread_group::TG_LIST;
use libkernel::error::{KernelError, Result};

pub fn sys_kill(pid: PidT, signal: UserSigId) -> Result<usize> {
    let signal: SigId = signal.try_into()?;

    let current_task = current_task();
    // Kill ourselves
    if pid == current_task.process.tgid.value() as PidT {
        current_task
            .process
            .signals
            .lock_save_irq()
            .set_pending(signal);
        return Ok(0);
    }

    match pid {
        p if p > 0 => {
            let target_tg = ThreadGroup::get(Tgid(p as _)).ok_or(KernelError::NoProcess)?;
            target_tg.signals.lock_save_irq().set_pending(signal);
        }

        0 => {
            let our_pgid = *current_task.process.pgid.lock_save_irq();
            // Iterate over all thread groups and signal the ones that are in
            // the same PGID.
            for tg_weak in crate::process::thread_group::TG_LIST
                .lock_save_irq()
                .values()
            {
                if let Some(tg) = tg_weak.upgrade()
                    && *tg.pgid.lock_save_irq() == our_pgid
                {
                    tg.signals.lock_save_irq().set_pending(signal);
                }
            }
        }

        p if p < 0 && p != -1 => {
            let target_pgid = Pgid((-p) as _);
            for tg_weak in crate::process::thread_group::TG_LIST
                .lock_save_irq()
                .values()
            {
                if let Some(tg) = tg_weak.upgrade()
                    && *tg.pgid.lock_save_irq() == target_pgid
                {
                    tg.signals.lock_save_irq().set_pending(signal);
                }
            }
        }

        _ => return Err(KernelError::NotSupported),
    }

    Ok(0)
}

pub fn sys_tkill(tid: PidT, signal: UserSigId) -> Result<usize> {
    let target_tid = Tid(tid as _);
    let current_task = current_task();

    let signal: SigId = signal.try_into()?;

    // The fast-path case.
    if current_task.tid == target_tid {
        current_task
            .process
            .signals
            .lock_save_irq()
            .set_pending(signal);
    } else {
        let task = current_task
            .process
            .tasks
            .lock_save_irq()
            .get(&target_tid)
            .and_then(|t| t.upgrade())
            .ok_or(KernelError::NoProcess)?;

        task.process.signals.lock_save_irq().set_pending(signal);
    }

    Ok(0)
}

pub fn send_signal_to_pg(pgid: Pgid, signal: SigId) {
    for tg_weak in TG_LIST.lock_save_irq().values() {
        if let Some(tg) = tg_weak.upgrade()
            && *tg.pgid.lock_save_irq() == pgid
        {
            tg.signals.lock_save_irq().set_pending(signal);
        }
    }
}
