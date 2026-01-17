use crate::clock::timespec::TimeSpec;
use crate::memory::uaccess::{UserCopyable, copy_to_user};
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
use super::{Pgid, ThreadGroup};

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

// TODO: more fields needed for full compatibility
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SigInfo {
    pub signo: i32,
    pub code: i32,
    pub errno: i32,
}

unsafe impl UserCopyable for SigInfo {}

// si_code values for SIGCHLD
const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;
const CLD_DUMPED: i32 = 3;
const CLD_STOPPED: i32 = 4;
const CLD_TRAPPED: i32 = 5;
const CLD_CONTINUED: i32 = 6;

#[derive(Clone, Copy, Debug)]
pub enum ChildState {
    NormalExit { code: u32 },
    SignalExit { signal: SigId, core: bool },
    Stop { signal: SigId },
    TraceTrap { signal: SigId, mask: i32 },
    Continue,
}

impl ChildState {
    fn matches_wait_flags(&self, flags: WaitFlags) -> bool {
        match self {
            ChildState::NormalExit { .. } | ChildState::SignalExit { .. } => {
                flags.contains(WaitFlags::WEXITED)
            }
            ChildState::Stop { .. } => flags.contains(WaitFlags::WSTOPPED),
            // Always wake up on a trace trap.
            ChildState::TraceTrap { .. } => true,
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
    } else if pid < -1 {
        // Wait for any child whose process group ID matches abs(pid)
        let target_pgid = Pgid((-pid) as u32);
        state.iter().find_map(|(k, v)| {
            if !v.matches_wait_flags(flags) {
                return None;
            }
            if let Some(tg) = ThreadGroup::get(*k) {
                if *tg.pgid.lock_save_irq() == target_pgid {
                    Some(*k)
                } else {
                    None
                }
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

// Non-consuming wait finder to support WNOWAIT
fn find_waitable(
    state: &BTreeMap<Tgid, ChildState>,
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
    } else if pid < -1 {
        let target_pgid = Pgid((-pid) as u32);
        state.iter().find_map(|(k, v)| {
            if !v.matches_wait_flags(flags) {
                return None;
            }
            if let Some(tg) = ThreadGroup::get(*k) {
                if *tg.pgid.lock_save_irq() == target_pgid {
                    Some(*k)
                } else {
                    None
                }
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

    state.get(&key).map(|v| (key, *v))
}

pub async fn sys_wait4(
    pid: PidT,
    stat_addr: TUA<i32>,
    flags: u32,
    rusage: TUA<RUsage>,
) -> Result<usize> {
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

    let child_proc_count = task.process.children.lock_save_irq().iter().count();

    let (tgid, child_state) = if child_proc_count == 0 || flags.contains(WaitFlags::WNOHANG) {
        // Special case for no children. See if there are any pending child
        // notification events without sleeping. If there are no children and no
        // pending events, return ECHILD.
        let mut ret = None;
        task.process.child_notifiers.inner.update(|s| {
            ret = do_wait(s, pid, flags);
            WakeupType::None
        });

        match ret {
            Some(ret) => ret,
            None if child_proc_count == 0 => return Err(KernelError::NoChildProcess),
            None => return Ok(0),
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
                copy_to_user(stat_addr, ((signal.user_id() as i32) << 8) | 0x7f).await?;
            }
            ChildState::TraceTrap { signal, mask } => {
                copy_to_user(
                    stat_addr,
                    ((signal.user_id() as i32) << 8) | 0x7f | mask << 8,
                )
                .await?;
            }
            ChildState::Continue => {
                copy_to_user(stat_addr, 0xffff).await?;
            }
        }
    }

    Ok(tgid.value() as _)
}

// idtype for waitid
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum IdType {
    P_ALL = 0,
    P_PID = 1,
    P_PGID = 2,
}

pub async fn sys_waitid(
    idtype: i32,
    id: PidT,
    infop: TUA<SigInfo>,
    options: u32,
    rusage: TUA<RUsage>,
) -> Result<usize> {
    let which = match idtype {
        0 => IdType::P_ALL,
        1 => IdType::P_PID,
        2 => IdType::P_PGID,
        _ => return Err(KernelError::InvalidValue),
    };

    let flags = WaitFlags::from_bits_retain(options);

    if flags.contains_unknown_bits() {
        return Err(KernelError::InvalidValue);
    }

    // Validate options subset allowed for waitid
    if !flags
        .difference(
            WaitFlags::WNOHANG
                | WaitFlags::WSTOPPED
                | WaitFlags::WCONTINUED
                | WaitFlags::WEXITED
                | WaitFlags::WNOWAIT,
        )
        .is_empty()
    {
        return Err(KernelError::InvalidValue);
    }

    if !rusage.is_null() {
        todo!();
    }

    // Map which/id to pid selection used by our wait helpers
    let sel_pid: PidT = match which {
        IdType::P_ALL => -1,
        IdType::P_PID => id,
        IdType::P_PGID => -id.abs(), // negative means select by PGID in helpers
    };

    let task = current_task_shared();
    let child_proc_count = task.process.children.lock_save_irq().iter().count();

    // Try immediate check if no children or WNOHANG
    let child_state = if child_proc_count == 0 || flags.contains(WaitFlags::WNOHANG) {
        let mut ret: Option<ChildState> = None;
        task.process.child_notifiers.inner.update(|s| {
            // Use non-consuming finder for WNOWAIT, else consume
            ret = if flags.contains(WaitFlags::WNOWAIT) {
                find_waitable(s, sel_pid, flags).map(|(_, state)| state)
            } else {
                do_wait(s, sel_pid, flags).map(|(_, state)| state)
            };
            WakeupType::None
        });

        match ret {
            Some(ret) => ret,
            None if child_proc_count == 0 => return Err(KernelError::NoChildProcess),
            None => return Ok(0),
        }
    } else {
        // Wait until a child matches; first find key, then remove conditionally
        let (_, state) = task
            .process
            .child_notifiers
            .inner
            .wait_until(|s| {
                if flags.contains(WaitFlags::WNOWAIT) {
                    find_waitable(s, sel_pid, flags)
                } else {
                    do_wait(s, sel_pid, flags)
                }
            })
            .await;
        state
    };

    // Populate siginfo
    if !infop.is_null() {
        let mut siginfo = SigInfo {
            signo: SigId::SIGCHLD.user_id() as i32,
            code: 0,
            errno: 0,
        };
        match child_state {
            ChildState::NormalExit { code } => {
                siginfo.code = CLD_EXITED;
                siginfo.errno = code as i32;
            }
            ChildState::SignalExit { signal, core } => {
                siginfo.code = if core { CLD_DUMPED } else { CLD_KILLED };
                siginfo.errno = signal.user_id() as i32;
            }
            ChildState::Stop { signal } => {
                siginfo.code = CLD_STOPPED;
                siginfo.errno = signal.user_id() as i32;
            }
            ChildState::TraceTrap { signal, .. } => {
                siginfo.code = CLD_TRAPPED;
                siginfo.errno = signal.user_id() as i32;
            }
            ChildState::Continue => {
                siginfo.code = CLD_CONTINUED;
            }
        }
        copy_to_user(infop, siginfo).await?;
    }

    // If WNOWAIT was specified, don't consume the state; our helpers already honored that
    // Return 0 on success
    Ok(0)
}
