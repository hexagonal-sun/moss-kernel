use core::{
    alloc::Layout,
    fmt::Display,
    mem::transmute,
    ops::{Index, IndexMut},
};

use bitflags::bitflags;
use ksigaction::{KSignalAction, UserspaceSigAction};
use libkernel::memory::{address::UA, region::UserMemoryRegion};

use crate::memory::uaccess::UserCopyable;

pub mod kill;
pub mod ksigaction;
pub mod sigaction;
pub mod sigaltstack;
pub mod sigprocmask;
mod uaccess;

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct SigSet: u64 {
       const SIGHUP     = 1 << 0;
       const SIGINT     = 1 << 1;
       const SIGQUIT    = 1 << 2;
       const SIGILL     = 1 << 3;
       const SIGTRAP    = 1 << 4;
       const SIGABRT    = 1 << 5;
       const SIGBUS     = 1 << 6;
       const SIGFPE     = 1 << 7;
       const SIGKILL    = 1 << 8;
       const SIGUSR1    = 1 << 9;
       const SIGSEGV    = 1 << 10;
       const SIGUSR2    = 1 << 11;
       const SIGPIPE    = 1 << 12;
       const SIGALRM    = 1 << 13;
       const SIGTERM    = 1 << 14;
       const SIGSTKFLT  = 1 << 15;
       const SIGCHLD    = 1 << 16;
       const SIGCONT    = 1 << 17;
       const SIGSTOP    = 1 << 18;
       const SIGTSTP    = 1 << 19;
       const SIGTTIN    = 1 << 20;
       const SIGTTOU    = 1 << 21;
       const SIGURG     = 1 << 22;
       const SIGXCPU    = 1 << 23;
       const SIGXFSZ    = 1 << 24;
       const SIGVTALRM  = 1 << 25;
       const SIGPROF    = 1 << 26;
       const SIGWINCH   = 1 << 27;
       const SIGIO      = 1 << 28;
       const SIGPWR     = 1 << 29;
       const SIGUNUSED  = 1 << 30;
    }
}

unsafe impl UserCopyable for SigSet {}

impl From<SigId> for SigSet {
    fn from(value: SigId) -> Self {
        Self::from_bits_retain(1 << value as u32)
    }
}

impl From<SigSet> for SigId {
    fn from(value: SigSet) -> Self {
        debug_assert_eq!(value.iter().count(), 1);

        let id = value.bits().trailing_zeros();

        if id > 30 {
            panic!("Unexpected signal id {id}");
        }

        // SAFETY: We have performed bounds checking above to ensure the value
        // is within the enum range
        unsafe { transmute(id) }
    }
}

impl SigSet {
    /// Set the signal with id `signal` to true in the set.
    pub fn set_signal(&mut self, signal: SigId) {
        *self = self.union(signal.into());
    }

    /// Remove a set signal from the set, setting it to false, while respecting
    /// `mask`. Returns the ID of the removed signal.
    pub fn take_signal(&mut self, mask: SigSet) -> Option<SigId> {
        let signal = self.difference(mask).iter().next()?;

        self.remove(signal);

        Some(signal.into())
    }
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(clippy::upper_case_acronyms)]
pub enum SigId {
    SIGHUP = 0,
    SIGINT = 1,
    SIGQUIT = 2,
    SIGILL = 3,
    SIGTRAP = 4,
    SIGABRT = 5,
    SIGBUS = 6,
    SIGFPE = 7,
    SIGKILL = 8,
    SIGUSR1 = 9,
    SIGSEGV = 10,
    SIGUSR2 = 11,
    SIGPIPE = 12,
    SIGALRM = 13,
    SIGTERM = 14,
    SIGSTKFLT = 15,
    SIGCHLD = 16,
    SIGCONT = 17,
    SIGSTOP = 18,
    SIGTSTP = 19,
    SIGTTIN = 20,
    SIGTTOU = 21,
    SIGURG = 22,
    SIGXCPU = 23,
    SIGXFSZ = 24,
    SIGVTALRM = 25,
    SIGPROF = 26,
    SIGWINCH = 27,
    SIGIO = 28,
    SIGPWR = 29,
    SIGUNUSED = 30,
}

impl SigId {
    pub fn user_id(self) -> u64 {
        self as u64 + 1
    }

    pub fn is_stopping(self) -> bool {
        matches!(
            self,
            Self::SIGSTOP | Self::SIGTSTP | Self::SIGTTIN | Self::SIGTTOU
        )
    }
}

impl Display for SigId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let set: SigSet = (*self).into();
        let name = set.iter_names().next().unwrap().0;
        f.write_str(name)
    }
}

// SIGKILL and SIGSTOP
const UNMASKABLE_SIGNALS: SigSet = SigSet::SIGKILL.union(SigSet::SIGSTOP);

#[derive(Clone, Copy, Debug)]
pub enum SigActionState {
    Ignore,
    Default,
    Action(UserspaceSigAction),
}

#[derive(Clone)]
pub struct SigActionSet([SigActionState; 64]);

impl Index<SigId> for SigActionSet {
    type Output = SigActionState;

    fn index(&self, index: SigId) -> &Self::Output {
        self.0.index(index as usize)
    }
}

impl IndexMut<SigId> for SigActionSet {
    fn index_mut(&mut self, index: SigId) -> &mut Self::Output {
        self.0.index_mut(index as usize)
    }
}

#[derive(Clone)]
pub struct AltSigStack {
    range: UserMemoryRegion,
    ptr: UA,
}

pub struct AltStackAlloc {
    pub old_ptr: UA,
    pub data_ptr: UA,
}

impl AltSigStack {
    pub fn alloc_alt_stack<T>(&mut self) -> Option<AltStackAlloc> {
        let layout = Layout::new::<T>();
        let old_ptr = self.ptr;
        let new_ptr = self.ptr.sub_bytes(layout.size()).align(layout.align());

        if !self.range.contains_address(new_ptr) {
            None
        } else {
            self.ptr = new_ptr;
            Some(AltStackAlloc {
                old_ptr,
                data_ptr: new_ptr,
            })
        }
    }

    pub fn restore_alt_stack(&mut self, old_ptr: UA) {
        self.ptr = old_ptr
    }

    pub fn in_use(&self) -> bool {
        self.ptr != self.range.end_address()
    }
}

#[derive(Clone)]
pub struct SignalActionState {
    action: SigActionSet,
    pub alt_stack: Option<AltSigStack>,
}

impl SignalActionState {
    pub fn new_ignore() -> Self {
        Self {
            action: SigActionSet([SigActionState::Ignore; 64]),
            alt_stack: None,
        }
    }

    pub fn new_default() -> Self {
        Self {
            action: SigActionSet([SigActionState::Default; 64]),
            alt_stack: None,
        }
    }

    pub fn action_signal(&self, id: SigId) -> Option<KSignalAction> {
        match self.action[id] {
            SigActionState::Ignore => None, // look for another signal,
            SigActionState::Default => KSignalAction::default_action(id),
            SigActionState::Action(userspace_sig_action) => {
                Some(KSignalAction::Userspace(id, userspace_sig_action))
            }
        }
    }
}
