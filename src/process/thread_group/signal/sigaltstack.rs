use crate::{
    memory::uaccess::{UserCopyable, copy_from_user, copy_to_user},
    sched::syscall_ctx::ProcessCtx,
};
use bitflags::bitflags;
use libkernel::{
    error::{KernelError, Result},
    memory::{
        address::{TUA, UA},
        region::UserMemoryRegion,
    },
};

use super::AltSigStack;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UserSigAltStack {
    ss_sp: UA,
    ss_flags: SigAltStackFlags,
    ss_size: usize,
}

unsafe impl UserCopyable for UserSigAltStack {}

bitflags! {
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct SigAltStackFlags: i32 {
        const SS_ONSTACK = 1;
        const SS_DISABLE = 2;
    }
}

impl From<UserSigAltStack> for Option<AltSigStack> {
    fn from(value: UserSigAltStack) -> Self {
        if value.ss_flags.contains(SigAltStackFlags::SS_DISABLE) {
            None
        } else {
            let range = UserMemoryRegion::new(value.ss_sp, value.ss_size);
            Some(AltSigStack {
                range,
                ptr: range.end_address(),
            })
        }
    }
}

const MIN_STACK_SZ: usize = 128;

impl From<Option<AltSigStack>> for UserSigAltStack {
    fn from(value: Option<AltSigStack>) -> Self {
        if let Some(value) = value {
            let mut flags = SigAltStackFlags::empty();

            if value.in_use() {
                flags.insert(SigAltStackFlags::SS_ONSTACK);
            }

            Self {
                ss_sp: value.range.start_address(),
                ss_flags: flags,
                ss_size: value.range.size(),
            }
        } else {
            Self {
                ss_sp: UA::null(),
                ss_flags: SigAltStackFlags::empty(),
                ss_size: 0,
            }
        }
    }
}

pub async fn sys_sigaltstack(
    ctx: &ProcessCtx,
    ss: TUA<UserSigAltStack>,
    old_ss: TUA<UserSigAltStack>,
) -> Result<usize> {
    let ss = if !ss.is_null() {
        Some(copy_from_user(ss).await?)
    } else {
        None
    };

    let old_ss_value = {
        let task = ctx.shared();
        let mut signals = task.process.signals.lock_save_irq();

        let old_ss_value = signals.alt_stack.clone();

        if let Some(ss) = ss {
            if ss.ss_size < MIN_STACK_SZ {
                Err(KernelError::NoMemory)?;
            }

            let new_ss: Option<AltSigStack> = ss.into();

            if new_ss.is_none()
                && let Some(old_ss) = old_ss_value.as_ref()
                && old_ss.in_use()
            {
                // We cannot disable an in use alt stack.
                Err(KernelError::NotPermitted)?;
            }

            signals.alt_stack = new_ss;
        }

        old_ss_value
    };

    if !old_ss.is_null() {
        copy_to_user(old_ss, old_ss_value.into()).await?;
    }

    Ok(0)
}
