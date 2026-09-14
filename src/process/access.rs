//! Cross-task access checks shared by procfs and process-control interfaces.
//!
//! Process IDs remain in the initial PID namespace; user namespace capability
//! authority is checked against the target's credential namespace.
use super::Task;
use core::sync::atomic::Ordering;
use libkernel::{
    error::{KernelError, Result},
    proc::caps::CapabilitiesFlags,
};

pub fn ptrace_may_access(caller: &Task, target: &Task, fscreds: bool) -> Result<()> {
    if caller.process.tgid == target.process.tgid {
        return Ok(());
    }
    let caller = caller.creds.lock_save_irq().clone();
    // Credential changes update dumpability under this same credential lock.
    let target_creds = target.creds.lock_save_irq();
    if caller.capable_in(&target_creds.user_ns(), CapabilitiesFlags::CAP_SYS_PTRACE) {
        return Ok(());
    }
    let (uid, gid, caps) = if fscreds {
        (caller.fsuid(), caller.fsgid(), caller.caps().effective())
    } else {
        (caller.uid(), caller.gid(), caller.caps().permitted())
    };
    if [target_creds.uid(), target_creds.euid(), target_creds.suid()]
        .iter()
        .any(|id| *id != uid)
        || [target_creds.gid(), target_creds.egid(), target_creds.sgid()]
            .iter()
            .any(|id| *id != gid)
        || target.process.dumpable.load(Ordering::Acquire) != 1
        || caller.user_ns() != target_creds.user_ns()
        || !caps.contains(target_creds.caps().permitted())
    {
        return Err(KernelError::NotPermitted);
    }
    Ok(())
}

pub fn signal_may_access(
    caller: &Task,
    target: &Task,
    signal: Option<super::thread_group::signal::SigId>,
) -> Result<()> {
    if caller.process.tgid == target.process.tgid {
        return Ok(());
    }
    let from = caller.creds.lock_save_irq().clone();
    let to = target.creds.lock_save_irq().clone();
    if [from.uid(), from.euid()]
        .iter()
        .any(|id| *id == to.uid() || *id == to.suid())
        || from.capable_in(&to.user_ns(), CapabilitiesFlags::CAP_KILL)
    {
        return Ok(());
    }
    if signal == Some(super::thread_group::signal::SigId::SIGCONT) {
        let from_sid = *caller.process.sid.lock_save_irq();
        let to_sid = *target.process.sid.lock_save_irq();
        if from_sid == to_sid {
            return Ok(());
        }
    }
    Err(KernelError::NotPermitted)
}
