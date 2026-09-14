//! Cross-task access checks shared by procfs and process-control interfaces.
//!
//! All tasks currently belong to the initial user/PID namespaces. Creating a
//! different namespace is rejected until ID maps and namespace-scoped capability
//! checks exist; a global capability must never grant authority in a fake namespace.
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
    if caller.caps().is_capable(CapabilitiesFlags::CAP_SYS_PTRACE) {
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
        || !caps.contains(target_creds.caps().permitted())
    {
        return Err(KernelError::NotPermitted);
    }
    Ok(())
}
