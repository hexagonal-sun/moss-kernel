use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::UA;
use crate::process::{find_task_by_descriptor, Task, Tid, TASK_LIST};
use crate::process::thread_group::signal::SigId;
use crate::sched::current::current_task_shared;

#[repr(i32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PtraceOperation {
    TraceMe = 0,
    PeekText = 1,
    PeekData = 2,
    PeekUser = 3,
    PokeText = 4,
    PokeData = 5,
    PokeUser = 6,
    Cont = 7,
    Kill = 8,
    SingleStep = 9,
    GetRegs = 12,
    SetRegs = 13,
    GetFpRegs = 14,
    SetFpRegs = 15,
    Attach = 16,
    Detach = 17,
    Syscall = 24,
}

impl TryFrom<i32> for PtraceOperation {
    type Error = KernelError;

    fn try_from(value: i32) -> Result<Self> {
        match value {
            0 => Ok(PtraceOperation::TraceMe),
            1 => Ok(PtraceOperation::PeekText),
            2 => Ok(PtraceOperation::PeekData),
            // TODO: Should be EIO
            _ => Err(KernelError::InvalidValue)
        }
    }
}

pub async fn sys_ptrace(op: i32, pid: Tid, addr: UA, data: UA) -> Result<usize> {
    let op = PtraceOperation::try_from(op)?;
    if op == PtraceOperation::TraceMe {
        // Change ptrace status
        let current_task = current_task_shared();
        current_task.ptrace.store(true, Ordering::SeqCst);
        return Ok(0);
    }
    let task_list = TASK_LIST.lock_save_irq();
    let id = task_list
        .iter()
        .find(|(desc, _)| desc.tid == pid)
        .map(|(desc, _)| *desc);
    drop(task_list);
    let task_details = if let Some(desc) = id {
        find_task_by_descriptor(&desc)
    } else {
        None
    };
    // TODO: Wrong error?
    let task = task_details.ok_or(KernelError::NoProcess)?;
    // Check if the current task is allowed to ptrace the target task
    let current_task = current_task_shared();
    // TODO: Check CAP_SYS_PTRACE & security
    if !current_task.process.children.lock_save_irq().iter().any(|(ch_id, _)| &task.process.tgid == ch_id) {
        // TODO: Wrong error
        return Err(KernelError::NotSupported);
    }
    match op {
        PtraceOperation::TraceMe => {
            unreachable!();
        }
        PtraceOperation::Attach => {
            if !task.ptrace.load(Ordering::SeqCst) {
                // TODO: Wrong error
                return Err(KernelError::InvalidValue);
            }
            task.process
                .signals
                .lock_save_irq()
                .set_pending(SigId::SIGSTOP);
            Ok(0)
        }
        // TODO: Wrong error
        _ => Err(KernelError::InvalidValue),
    }
}