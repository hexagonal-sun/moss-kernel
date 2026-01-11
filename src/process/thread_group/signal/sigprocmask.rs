use crate::memory::uaccess::{copy_from_user, copy_to_user};
use crate::sched::current::current_task;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::TUA;

use super::{SigSet, UNMASKABLE_SIGNALS};

pub const SIG_BLOCK: u32 = 0;
pub const SIG_UNBLOCK: u32 = 1;
pub const SIG_SETMASK: u32 = 2;

pub async fn sys_rt_sigprocmask(
    how: u32,
    set: TUA<SigSet>,
    oldset: TUA<SigSet>,
    sigset_size: usize,
) -> Result<usize> {
    if sigset_size != size_of::<SigSet>() {
        return Err(KernelError::InvalidValue);
    }

    let set = if !set.is_null() {
        Some(copy_from_user(set).await?)
    } else {
        None
    };

    let old_sigmask = {
        let mut task = current_task();
        let old_sigmask = task.sig_mask;

        if let Some(set) = set {
            let mut new_sigmask = match how {
                SIG_BLOCK => old_sigmask.union(set),
                SIG_UNBLOCK => old_sigmask.difference(set),
                SIG_SETMASK => set,
                _ => return Err(KernelError::InvalidValue),
            };

            // SIGSTOP and SIGKILL can never be masked.
            new_sigmask.remove(UNMASKABLE_SIGNALS);

            task.sig_mask = new_sigmask;
        }

        old_sigmask
    };

    if !oldset.is_null() {
        copy_to_user(oldset, old_sigmask).await?;
    }

    Ok(0)
}
