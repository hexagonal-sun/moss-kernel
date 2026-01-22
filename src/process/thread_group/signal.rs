use crate::{memory::uaccess::UserCopyable, sched::current::current_task};
use bitflags::bitflags;
use core::{
    alloc::Layout,
    fmt::Display,
    mem::transmute,
    ops::{Index, IndexMut},
    task::Poll,
};
use ksigaction::{KSignalAction, UserspaceSigAction};
use libkernel::memory::{address::UA, region::UserMemoryRegion};

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
        let signal = self.peek_signal(mask)?;

        self.remove(signal.into());

        Some(signal)
    }

    /// Check whether a signal is set in this set while repseciting the signal
    /// mask, `mask`. Returns the ID of the set signal.
    pub fn peek_signal(&self, mask: SigSet) -> Option<SigId> {
        self.difference(mask).iter().next().map(|x| x.into())
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

pub trait Interruptable<T, F: Future<Output = T>> {
    /// Mark this operation as interruptable.
    ///
    /// When a signal is delivered to this process/task while it is `Sleeping`,
    /// it may be woken up if there are no running tasks to deliver the signal
    /// to. If a task is running an `interruptable()` future, then the
    /// underlying future's execution will be short-circuted by the delivery of
    /// a signal. If the kernel is running a non-`interruptable()` future, then
    /// the signal delivery is deferred until either an `interruptable()`
    /// operation is executed or the system call has finished.
    ///
    /// `.await`ing a `interruptable()`-wrapped future returns a
    /// [InterruptResult].
    fn interruptable(self) -> InterruptableFut<T, F>;
}

/// A wrapper for a long-running future, allowing it to be interrupted by a
/// signal.
pub struct InterruptableFut<T, F: Future<Output = T>> {
    sub_fut: F,
}

impl<T, F: Future<Output = T>> Interruptable<T, F> for F {
    fn interruptable(self) -> InterruptableFut<T, F> {
        // TODO: Set the task state to a new variant `Interruptable`. This
        // allows the `deliver_signal` code to wake up a task to deliver a
        // signal to where it will be actioned.
        InterruptableFut { sub_fut: self }
    }
}

impl<T, F: Future<Output = T>> Future for InterruptableFut<T, F> {
    type Output = InterruptResult<T>;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> Poll<Self::Output> {
        // Try the underlying future first.
        let res = unsafe {
            self.map_unchecked_mut(|f| &mut f.sub_fut)
                .poll(cx)
                .map(|x| InterruptResult::Uninterrupted(x))
        };

        if res.is_ready() {
            return res;
        }

        // See if there's a pending signal which interrupts this future.
        if current_task().peek_signal().is_some() {
            Poll::Ready(InterruptResult::Interrupted)
        } else {
            Poll::Pending
        }
    }
}

/// The result of running an interruptable operation within the kernel.
pub enum InterruptResult<T> {
    /// The operation was interrupted due to the delivery of the specified
    /// signal. The system call would normally short-circuit and return -EINTR
    /// at this point.
    Interrupted,
    /// The underlying future completed without interruption.
    Uninterrupted(T),
}
