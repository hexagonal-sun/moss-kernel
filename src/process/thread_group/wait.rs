use crate::clock::timespec::TimeSpec;
use crate::memory::uaccess::copy_to_user;
use crate::sched::current::current_task_shared;
use crate::sync::CondVar;
use alloc::collections::btree_map::BTreeMap;
use bitflags::Flags;
use libkernel::sync::condvar::WakeupType;
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

use super::Tgid;
use super::signal::SigId;

pub type PidT = i32;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RUsage {
    pub ru_utime: TimeSpec, // user time used
    pub ru_stime: TimeSpec, // system time used
    pub ru_maxrss: i64,     // maximum resident set size
    pub ru_ixrss: i64,      // integral shared memory size
    pub ru_idrss: i64,      // integral unshared data size
    pub ru_isrss: i64,      // integral unshared stack size
    pub ru_minflt: i64,     // page reclaims
    pub ru_majflt: i64,     // page faults
    pub ru_nswap: i64,      // swaps
    pub ru_inblock: i64,    // block input operations
    pub ru_oublock: i64,    // block output operations
    pub ru_msgsnd: i64,     // messages sent
    pub ru_msgrcv: i64,     // messages received
    pub ru_nsignals: i64,   // signals received
    pub ru_nvcsw: i64,      // voluntary context switches
    pub ru_nivcsw: i64,     // involuntary context switches
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct WaitFlags: u32 {
       const WNOHANG    = 0x00000001;
       const WSTOPPED   = 0x00000002;
       const WEXITED    = 0x00000004;
       const WCONTINUED = 0x00000008;
       const WNOWAIT    = 0x10000000;
       const WNOTHREAD  = 0x20000000;
       const WALL       = 0x40000000;
       const WCLONE     = 0x80000000;
    }
}

pub enum ChildState {
    NormalExit { code: u32 },
    SignalExit { signal: SigId, core: bool },
    Stop { signal: SigId },
    Continue,
}

impl ChildState {
    fn matches_wait_flags(&self, flags: WaitFlags) -> bool {
        match self {
            ChildState::NormalExit { .. } | ChildState::SignalExit { .. } => {
                flags.contains(WaitFlags::WEXITED)
            }
            ChildState::Stop { .. } => flags.contains(WaitFlags::WSTOPPED),
            ChildState::Continue => flags.contains(WaitFlags::WCONTINUED),
        }
    }
}

pub struct ChildNotifiers {
    inner: CondVar<BTreeMap<Tgid, ChildState>>,
}

impl Default for ChildNotifiers {
    fn default() -> Self {
        Self::new()
    }
}

impl ChildNotifiers {
    pub fn new() -> Self {
        Self {
            inner: CondVar::new(BTreeMap::new()),
        }
    }

    pub fn child_update(&self, tgid: Tgid, new_state: ChildState) {
        self.inner.update(|state| {
            state.insert(tgid, new_state);

            // Since some wakers may be conditional upon state update changes,
            // notify everyone whenever a child updates it's state.
            WakeupType::All
        });
    }
}

fn do_wait(
    state: &mut BTreeMap<Tgid, ChildState>,
    pid: PidT,
    flags: WaitFlags,
) -> Option<(Tgid, ChildState)> {
    let key = if pid == -1 {
        state.iter().find_map(|(k, v)| {
            if v.matches_wait_flags(flags) {
                Some(*k)
            } else {
                None
            }
        })
    } else {
        state
            .get_key_value(&Tgid::from_pid_t(pid))
            .and_then(|(k, v)| {
                if v.matches_wait_flags(flags) {
                    Some(*k)
                } else {
                    None
                }
            })
    }?;

    Some(state.remove_entry(&key).unwrap())
}

pub async fn sys_wait4(
    pid: PidT,
    stat_addr: TUA<i32>,
    flags: u32,
    rusage: TUA<RUsage>,
) -> Result<usize> {
    if pid < -1 {
        // TODO: Funky waiting.
        return Err(KernelError::NotSupported);
    }

    let mut flags = WaitFlags::from_bits_retain(flags);

    if flags.contains_unknown_bits() {
        return Err(KernelError::InvalidValue);
    }

    // Check for valid flags.
    if !flags
        .difference(
            WaitFlags::WNOHANG
                | WaitFlags::WSTOPPED
                | WaitFlags::WCONTINUED
                | WaitFlags::WNOTHREAD
                | WaitFlags::WCLONE
                | WaitFlags::WALL,
        )
        .is_empty()
    {
        return Err(KernelError::InvalidValue);
    }

    // wait4 implies WEXITED.
    flags.insert(WaitFlags::WEXITED);

    if !rusage.is_null() {
        // TODO: Funky waiting.
        return Err(KernelError::NotSupported);
    }

    let task = current_task_shared();

    let (tgid, child_state) = if flags.contains(WaitFlags::WNOHANG) {
        let mut ret = None;
        task.process.child_notifiers.inner.update(|s| {
            ret = do_wait(s, pid, flags);
            WakeupType::None
        });

        match ret {
            None => return Ok(0),
            Some(ret) => ret,
        }
    } else {
        task.process
            .child_notifiers
            .inner
            .wait_until(|state| do_wait(state, pid, flags))
            .await
    };

    if !stat_addr.is_null() {
        match child_state {
            ChildState::NormalExit { code } => {
                copy_to_user(stat_addr, (code as i32 & 0xff) << 8).await?;
            }
            ChildState::SignalExit { signal, core } => {
                copy_to_user(
                    stat_addr,
                    (signal.user_id() as i32) | if core { 0x80 } else { 0x0 },
                )
                .await?;
            }
            ChildState::Stop { signal } => {
                copy_to_user(stat_addr, ((signal as i32) << 8) | 0x7f).await?;
            }
            ChildState::Continue => {
                copy_to_user(stat_addr, 0xffff).await?;
            }
        }
    }

    Ok(tgid.value() as _)
}
