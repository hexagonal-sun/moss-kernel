use bitflags::bitflags;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::TUA;

use crate::memory::uaccess::{UserCopyable, copy_from_user, copy_to_user};
use crate::sched::current::current_task;

use super::ksigaction::UserspaceSigAction;
use super::uaccess::UserSigId;
use super::{SigActionState, SigId, SigSet};

bitflags! {
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct SigActionFlags: u64 {
        const SA_NOCLDSTOP      = 1 << 1;
        const SA_NOCLDWAIT      = 1 << 2;
        const SA_SIGINFO        = 1 << 3;
        const SA_UNSUPPORTED    = 1 << 10;
        const SA_EXPOSE_TAGBITS = 1 << 11;
        const SA_RESTORER       = 1 << 26;
        const SA_ONSTACK        = 1 << 27;
        const SA_RESTART        = 1 << 28;
        const SA_NODEFER        = 1 << 30;
        const SA_RESETHAND      = 1 << 31;
    }
}

const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UserSigAction {
    pub sa_sigaction: TUA<extern "C" fn(i32)>,
    pub sa_flags: SigActionFlags,
    pub sa_restorer: TUA<extern "C" fn()>,
    pub sa_mask: SigSet,
}

unsafe impl UserCopyable for UserSigAction {}

impl From<UserSigAction> for SigActionState {
    fn from(value: UserSigAction) -> Self {
        match value.sa_sigaction.value() {
            SIG_DFL => return Self::Default,
            SIG_IGN => return Self::Ignore,
            _ => {}
        }

        let restorer = if value.sa_flags.contains(SigActionFlags::SA_RESTORER) {
            Some(value.sa_restorer)
        } else {
            None
        };

        Self::Action(UserspaceSigAction {
            action: value.sa_sigaction,
            restorer,
            flags: value.sa_flags,
            mask: value.sa_mask,
        })
    }
}

impl From<SigActionState> for UserSigAction {
    fn from(value: SigActionState) -> Self {
        let mut ret = UserSigAction {
            sa_sigaction: TUA::null(),
            sa_flags: SigActionFlags::empty(),
            sa_restorer: TUA::null(),
            sa_mask: SigSet::empty(),
        };

        match value {
            SigActionState::Default => ret.sa_sigaction = TUA::from_value(SIG_DFL),
            SigActionState::Ignore => ret.sa_sigaction = TUA::from_value(SIG_IGN),
            SigActionState::Action(ua) => {
                ret.sa_sigaction = ua.action;
                ret.sa_restorer = ua.restorer.unwrap_or(TUA::null());
                ret.sa_flags = ua.flags;
                ret.sa_mask = ua.mask;
            }
        }

        ret
    }
}

pub async fn sys_rt_sigaction(
    sig: UserSigId,
    act: TUA<UserSigAction>,
    oact: TUA<UserSigAction>,
    sigsetsize: usize,
) -> Result<usize> {
    if sigsetsize != size_of::<SigSet>() {
        return Err(KernelError::InvalidValue)?;
    }

    let sig: SigId = sig.try_into()?;

    if sig == SigId::SIGKILL || sig == SigId::SIGSTOP {
        return Err(KernelError::InvalidValue);
    }

    let new_act = if !act.is_null() {
        Some(copy_from_user(act).await?)
    } else {
        None
    };

    let old_action = {
        let task = current_task();

        let mut sigstate = task.process.signals.lock_save_irq();
        let old_action = sigstate.action[sig];

        if let Some(new_action) = new_act {
            sigstate.action[sig] = new_action.into();
        }

        old_action
    };

    if !oact.is_null() {
        copy_to_user(oact, old_action.into()).await?;
    }

    Ok(0)
}
