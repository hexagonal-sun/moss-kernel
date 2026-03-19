use crate::arch::{Arch, ArchImpl};
use crate::memory::uaccess::{copy_from_user_slice, copy_to_user_slice};
use crate::process::thread_group::pid::PidT;
use crate::sched::sched_task::CPU_MASK_SIZE;
use crate::sched::syscall_ctx::ProcessCtx;
use crate::sched::{current_work, schedule};
use alloc::vec;
use libkernel::memory::address::UA;

pub fn sys_sched_yield() -> libkernel::error::Result<usize> {
    schedule();
    Ok(0)
}

pub async fn sys_sched_getaffinity(
    _ctx: &ProcessCtx,
    pid: PidT,
    size: usize,
    mask: UA,
) -> libkernel::error::Result<usize> {
    let task = if pid == 0 {
        current_work()
    } else {
        // TODO: Support getting affinity of other tasks if PERM_NICE
        return Err(libkernel::error::KernelError::InvalidValue);
    };
    let cpu_mask = {
        let sched_data = task.sched_data.lock_save_irq();
        sched_data.as_ref().unwrap().cpu_mask
    };
    let mut cpu_mask: &[u8] = &cpu_mask;
    if CPU_MASK_SIZE > size {
        cpu_mask = &cpu_mask[..size];
    }
    copy_to_user_slice(cpu_mask, mask).await?;
    Ok(cpu_mask.len())
}

pub async fn sys_sched_setaffinity(
    _ctx: &ProcessCtx,
    pid: PidT,
    size: usize,
    mask: UA,
) -> libkernel::error::Result<usize> {
    let mut cpu_set = vec![0u8; size];
    copy_from_user_slice(mask, cpu_set.as_mut_slice()).await?;
    let task = if pid == 0 {
        current_work()
    } else {
        // TODO: Support setting affinity of other tasks if PERM_NICE
        return Err(libkernel::error::KernelError::InvalidValue);
    };
    let mut sched_data = task.sched_data.lock_save_irq();
    if CPU_MASK_SIZE > size {
        return Err(libkernel::error::KernelError::InvalidValue);
    }
    cpu_set.truncate(CPU_MASK_SIZE);
    // Check if this turns off all CPUs, which is not allowed.
    let mut any_true = false;
    for i in 0..ArchImpl::cpu_count() {
        let byte_index = i / 8;
        let bit_index = i % 8;
        if (cpu_set[byte_index] & (1 << bit_index)) != 0 {
            any_true = true;
            break;
        }
    }
    if !any_true {
        return Err(libkernel::error::KernelError::InvalidValue);
    }
    sched_data.as_mut().unwrap().cpu_mask = cpu_set.try_into().unwrap();
    // TODO: apply the new affinity immediately if the current CPU is no longer in the set
    Ok(0)
}
