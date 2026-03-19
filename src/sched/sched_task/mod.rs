use core::{
    cmp::Ordering,
    ops::{Deref, DerefMut},
};

use super::{DEFAULT_TIME_SLICE, SCHED_WEIGHT_BASE, VT_FIXED_SHIFT};
use crate::{
    arch::{Arch, ArchImpl},
    drivers::timer::{Instant, schedule_preempt},
    process::{Task, owned::OwnedTask},
    sync::SpinLock,
};

use alloc::{boxed::Box, sync::Arc};
use state::TaskStateMachine;

pub mod state;

pub const NR_CPUS: usize = 256;
pub const CPU_MASK_SIZE: usize = NR_CPUS / 8;
pub type CpuMask = [u8; CPU_MASK_SIZE];

#[derive(Clone)]
pub struct SchedulerData {
    pub v_runtime: u128,
    /// Virtual time at which the task becomes eligible (v_ei).
    pub v_eligible: u128,
    /// Virtual deadline (v_di) used by the EEVDF scheduler.
    pub v_deadline: u128,
    pub exec_start: Option<Instant>,
    pub deadline: Option<Instant>,
    pub last_run: Option<Instant>,
    pub last_cpu: usize,
    pub cpu_mask: CpuMask,
    pub priority: i8,
}

impl SchedulerData {
    fn new(task: &OwnedTask) -> Self {
        Self {
            v_runtime: 0,
            v_eligible: 0,
            v_deadline: 0,
            exec_start: None,
            deadline: None,
            last_run: None,
            last_cpu: usize::MAX,
            cpu_mask: [u8::MAX; CPU_MASK_SIZE],
            priority: task.priority(),
        }
    }
}

pub struct Work {
    pub task: Box<OwnedTask>,
    pub state: TaskStateMachine,
    pub sched_data: SpinLock<Option<SchedulerData>>,
}

impl Deref for Work {
    type Target = Arc<Task>;

    fn deref(&self) -> &Self::Target {
        &self.task.t_shared
    }
}

impl DerefMut for Work {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.task.t_shared
    }
}

impl Work {
    pub fn new(task: Box<OwnedTask>) -> Arc<Self> {
        let sched_data = SchedulerData::new(&task);

        Arc::new(Self {
            task,
            state: TaskStateMachine::new(),
            sched_data: SpinLock::new(Some(sched_data)),
        })
    }

    pub fn into_runnable(self: Arc<Self>) -> RunnableTask {
        let mut sd = self
            .sched_data
            .lock_save_irq()
            .take()
            .expect("Should have sched data");

        // Refresh priority.
        sd.priority = self.task.priority();

        RunnableTask {
            work: self,
            sched_data: sd,
        }
    }
}

pub struct RunnableTask {
    pub(super) work: Arc<Work>,
    pub(super) sched_data: SchedulerData,
}

impl Drop for RunnableTask {
    // Replace the hot sched info back into the struct.
    fn drop(&mut self) {
        *self.work.sched_data.lock_save_irq() = Some(self.sched_data.clone());
    }
}

impl Deref for RunnableTask {
    type Target = SchedulerData;

    fn deref(&self) -> &Self::Target {
        &self.sched_data
    }
}

impl DerefMut for RunnableTask {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sched_data
    }
}

impl RunnableTask {
    /// Re-issue a virtual deadline
    fn replenish_deadline(&mut self) {
        let q_ns: u128 = DEFAULT_TIME_SLICE.as_nanos();
        let v_delta = (q_ns << VT_FIXED_SHIFT) / self.weight() as u128;
        self.v_deadline = self.v_eligible + v_delta;
    }

    /// Update accounting info for this task given the latest time. Returns
    /// `true` when we should try to reschedule another task, `false` otherwise.
    pub fn tick(&mut self, now: Instant) -> bool {
        let dv_increment = if let Some(start) = self.exec_start {
            let delta = now - start;
            let w = self.weight() as u128;
            ((delta.as_nanos()) << VT_FIXED_SHIFT) / w
        } else {
            0
        };

        self.v_runtime = self.v_runtime.saturating_add(dv_increment);

        // Advance its eligible time by the virtual run time it just used
        // (EEVDF: v_ei += t_used / w_i).
        self.v_eligible = self.v_eligible.saturating_add(dv_increment);

        self.exec_start = Some(now);

        // Has the task exceeded its deadline?
        if self.v_eligible >= self.v_deadline {
            self.replenish_deadline();

            true
        } else {
            // Task still has budget. Do nothing. Return to userspace
            // immediately.
            false
        }
    }

    /// Compute this task's scheduling weight.
    ///
    /// weight = priority + SCHED_WEIGHT_BASE
    /// The sum is clamped to a minimum of 1
    pub fn weight(&self) -> u32 {
        let w = self.sched_data.priority as i32 + SCHED_WEIGHT_BASE;
        if w <= 0 { 1 } else { w as u32 }
    }

    pub fn compare_with(&self, other: &Self) -> core::cmp::Ordering {
        self.v_deadline
            .cmp(&other.v_deadline)
            .then_with(|| self.v_runtime.cmp(&other.v_runtime))
            // If completely equal, prefer the one that hasn't run in a while?
            // Or prefer the one already running to avoid cache thrashing?
            // Usually irrelevant for EEVDF but strict ordering is good for
            // stability.
            .then_with(|| match (self.last_run, other.last_run) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
    }

    /// Update accounting information when the task is about to be inserted into
    /// a runqueue.
    pub fn inserting_into_runqueue(&mut self, vclock: u128) {
        // A freshly enqueued task becomes eligible immediately.
        self.v_eligible = vclock;

        // Grant it an initial virtual deadline proportional to its weight.
        let q_ns: u128 = DEFAULT_TIME_SLICE.as_nanos();
        let v_delta = (q_ns << VT_FIXED_SHIFT) / self.weight() as u128;
        self.v_deadline = vclock + v_delta;

        // Since the task is not executing yet, its exec_start must be `None`.
        self.exec_start = None;
    }

    /// Setup task accounting info such that it is about to be executed.
    pub fn about_to_execute(&mut self, now: Instant) {
        self.exec_start = Some(now);
        self.work.state.activate();

        // Deadline logic
        if self.deadline.is_none_or(|d| d <= now + DEFAULT_TIME_SLICE) {
            self.deadline = Some(now + DEFAULT_TIME_SLICE);
        }

        if let Some(d) = self.deadline {
            schedule_preempt(d);
        }
    }

    pub fn switch_context(&self) {
        ArchImpl::context_switch(self.work.task.t_shared.clone());
    }

    pub fn refresh_priority(&mut self) {
        self.sched_data.priority = self.work.task.priority();
    }
}
