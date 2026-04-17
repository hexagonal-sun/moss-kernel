use super::Tid;
use crate::{
    memory::uaccess::UserCopyable,
    sched::{
        sched_task::{Work, state::TaskState},
        waker::create_waker,
    },
    sync::SpinLock,
};
use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use builder::ThreadGroupBuilder;
use core::sync::atomic::AtomicUsize;
use core::{fmt::Display, sync::atomic::Ordering};
use libkernel::fs::pathbuf::PathBuf;
use pid::PidT;
use rsrc_lim::ResourceLimits;
use signal::{SigId, SigSet, SignalActionState};
use wait::Notifiers;

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

    fn from_tid(tid: Tid) -> Tgid {
        Self(tid.0)
    }

    fn from_pid_t(pid: PidT) -> Tgid {
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
    pub tasks: SpinLock<BTreeMap<Tid, Weak<Work>>>,
    pub signals: Arc<SpinLock<SignalActionState>>,
    pub rsrc_lim: Arc<SpinLock<ResourceLimits>>,
    pub pending_signals: SpinLock<SigSet>,
    pub priority: SpinLock<i8>,
    pub child_notifiers: Notifiers,
    pub utime: AtomicUsize,
    pub stime: AtomicUsize,
    pub last_account: AtomicUsize,
    pub executable: SpinLock<Option<PathBuf>>,
}

unsafe impl Send for ThreadGroup {}

impl ThreadGroup {
    pub fn new_child(self: Arc<Self>, share_state: bool, tid: Tid) -> Arc<ThreadGroup> {
        let mut builder = ThreadGroupBuilder::new(Tgid::from_tid(tid)).with_parent(self.clone());

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

        new_tg.clone()
    }

    pub fn get(id: Tgid) -> Option<Arc<Self>> {
        TG_LIST.lock_save_irq().get(&id).and_then(|x| x.upgrade())
    }

    pub fn notify_signal_waiters(&self) {
        let tasks: Vec<_> = self
            .tasks
            .lock_save_irq()
            .values()
            .filter_map(|task| task.upgrade())
            .collect();

        for task in tasks {
            task.notify_signal_waiters();
        }
    }

    pub fn queue_signal(&self, signal: SigId) {
        self.pending_signals.lock_save_irq().set_signal(signal);
        self.notify_signal_waiters();
    }

    pub fn set_pending_signals(&self, signals: SigSet) {
        *self.pending_signals.lock_save_irq() = signals;
        self.notify_signal_waiters();
    }

    pub fn deliver_signal(&self, signal: SigId) {
        match signal {
            SigId::SIGKILL => {
                // Set the sigkill marker in the pending signals and wake up all
                // tasks in this group.
                self.set_pending_signals(SigSet::SIGKILL);

                for task in self.tasks.lock_save_irq().values() {
                    if let Some(task) = task.upgrade() {
                        // Wake will handle Sleeping/Stopped → Enqueue,
                        // and Running/Pending* → PreventedSleep (sets Woken).
                        create_waker(task).wake();
                    }
                }
            }
            _ => {
                self.queue_signal(signal);

                // See whether there is a task that can action the signal.
                for task in self.tasks.lock_save_irq().values() {
                    if let Some(task) = task.upgrade()
                        && matches!(
                            task.state.load(Ordering::Acquire),
                            TaskState::Runnable
                                | TaskState::Running
                                | TaskState::Woken
                                | TaskState::PendingSleep
                                | TaskState::PendingStop
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
                        create_waker(task).wake();
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

static TG_LIST: SpinLock<BTreeMap<Tgid, Weak<ThreadGroup>>> = SpinLock::new(BTreeMap::new());
