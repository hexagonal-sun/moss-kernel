use super::{Task, TaskState, Tid};
use crate::{memory::uaccess::UserCopyable, sched::waker::create_waker, sync::SpinLock};
use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
};
use builder::ThreadGroupBuilder;
use core::sync::atomic::AtomicU64;
use core::{
    fmt::Display,
    sync::atomic::{AtomicU32, Ordering},
};
use pid::PidT;
use rsrc_lim::ResourceLimits;
use signal::{SigId, SigSet, SignalActionState};
use wait::ChildNotifiers;

pub mod builder;
pub mod pid;
pub mod rsrc_lim;
pub mod signal;
pub mod umask;
pub mod wait;

/// Task Group ID. In user-space this is the same as a Process ID (PID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tgid(pub u32);

impl Tgid {
    pub fn value(self) -> u32 {
        self.0
    }

    pub fn is_idle(self) -> bool {
        self.0 == 0
    }

    pub fn is_init(self) -> bool {
        self.0 == 1
    }

    pub fn init() -> Self {
        Self(1)
    }

    pub fn idle() -> Tgid {
        Self(0)
    }

    pub fn from_pid_t(pid: PidT) -> Tgid {
        Self(pid as _)
    }
}

impl Display for Tgid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

/// Process Group ID.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pgid(pub u32);

impl Pgid {
    pub fn value(self) -> u32 {
        self.0
    }
}

unsafe impl UserCopyable for Pgid {}

/// Session ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Sid(pub u32);

impl Sid {
    pub fn value(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Running, // Actively running
    Exiting, // In the middle of being torn down
}

pub struct ThreadGroup {
    pub tgid: Tgid,
    pub pgid: SpinLock<Pgid>,
    pub sid: SpinLock<Sid>,
    pub state: SpinLock<ProcessState>,
    pub umask: SpinLock<u32>,
    pub parent: SpinLock<Option<Weak<ThreadGroup>>>,
    pub children: SpinLock<BTreeMap<Tgid, Arc<ThreadGroup>>>,
    pub tasks: SpinLock<BTreeMap<Tid, Weak<Task>>>,
    pub signals: Arc<SpinLock<SignalActionState>>,
    pub rsrc_lim: Arc<SpinLock<ResourceLimits>>,
    pub pending_signals: SpinLock<SigSet>,
    pub priority: SpinLock<i8>,
    pub child_notifiers: ChildNotifiers,
    pub utime: AtomicU64,
    pub stime: AtomicU64,
    next_tid: AtomicU32,
}

unsafe impl Send for ThreadGroup {}

impl ThreadGroup {
    // Return the next avilable thread id. Will never return a thread who's ID
    // == TGID, since that is defined as the main, root thread.
    pub fn next_tid(&self) -> Tid {
        let mut v = self.next_tid.fetch_add(1, Ordering::Relaxed);

        // Skip the TGID.
        if v == self.tgid.value() {
            v = self.next_tid.fetch_add(1, Ordering::Relaxed);
        }

        Tid(v)
    }

    pub fn next_tgid() -> Tgid {
        Tgid(NEXT_TGID.fetch_add(1, Ordering::SeqCst))
    }

    pub fn new_child(self: Arc<Self>, share_state: bool) -> (Arc<ThreadGroup>, Tid) {
        let mut builder = ThreadGroupBuilder::new(Self::next_tgid()).with_parent(self.clone());

        if share_state {
            builder = builder
                .with_sigstate(self.signals.clone())
                .with_rsrc_lim(self.rsrc_lim.clone());
        } else {
            builder = builder
                .with_sigstate(Arc::new(SpinLock::new(
                    self.signals.lock_save_irq().clone(),
                )))
                .with_rsrc_lim(Arc::new(SpinLock::new(
                    self.rsrc_lim.lock_save_irq().clone(),
                )));
        }

        let new_tg = builder.build();

        self.children
            .lock_save_irq()
            .insert(new_tg.tgid, new_tg.clone());

        (new_tg.clone(), Tid(new_tg.tgid.value()))
    }

    pub fn get(id: Tgid) -> Option<Arc<Self>> {
        TG_LIST.lock_save_irq().get(&id).and_then(|x| x.upgrade())
    }

    pub fn deliver_signal(&self, signal: SigId) {
        match signal {
            SigId::SIGKILL => {
                // Set the sigkill marker in the pending signals and wake up all
                // tasks in this group.
                *self.pending_signals.lock_save_irq() = SigSet::SIGKILL;

                for task in self.tasks.lock_save_irq().values() {
                    if let Some(task) = task.upgrade()
                        && matches!(
                            *task.state.lock_save_irq(),
                            TaskState::Stopped | TaskState::Sleeping
                        )
                    {
                        create_waker(task.descriptor()).wake();
                    }
                }
            }
            _ => {
                self.pending_signals.lock_save_irq().set_signal(signal);

                // See whether there is a task that can action the signal.
                for task in self.tasks.lock_save_irq().values() {
                    if let Some(task) = task.upgrade()
                        && matches!(
                            *task.state.lock_save_irq(),
                            TaskState::Runnable | TaskState::Running
                        )
                    {
                        // Signal delivered. This task will eventually be
                        // dispatched again by the uspc_ret code and the
                        // signal picked up.
                        return;
                    }
                }

                // No task will pick up the signal. Wake one up.
                for task in self.tasks.lock_save_irq().values() {
                    if let Some(task) = task.upgrade() {
                        create_waker(task.descriptor()).wake();
                        return;
                    }
                }
            }
        }
    }
}

impl Drop for ThreadGroup {
    fn drop(&mut self) {
        TG_LIST.lock_save_irq().remove(&self.tgid);
    }
}

// the idle process (0) and the init process (1) are allocated manually.
static NEXT_TGID: AtomicU32 = AtomicU32::new(2);

static TG_LIST: SpinLock<BTreeMap<Tgid, Weak<ThreadGroup>>> = SpinLock::new(BTreeMap::new());
