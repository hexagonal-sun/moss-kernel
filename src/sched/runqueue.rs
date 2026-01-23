use core::cmp::Ordering;

use crate::{
    drivers::timer::Instant,
    process::{TaskDescriptor, TaskState},
};
use alloc::{boxed::Box, collections::btree_map::BTreeMap};
use log::warn;

use super::{VCLOCK_EPSILON, sched_task::SchedulableTask};

/// The result of a requested task switch.
pub enum SwitchResult {
    /// The requested task is already running. No changes made.
    AlreadyRunning,
    /// A switch occurred. The previous task was Runnable and has been
    /// re-queued.
    Preempted,
    /// A switch occurred. The previous task is Blocked (or Finished) and
    /// ownership is returned to the caller (to handle sleep/wait queues).
    Blocked { old_task: Box<SchedulableTask> },
}

/// A simple weight-tracking runqueue.
///
/// Invariants:
/// 1. `total_weight` = Sum(queue tasks) + Weight(running_task) (excluding the idle task).
/// 2. `running_task` is NOT in `queue`.
pub struct RunQueue {
    total_weight: u64,
    pub(super) queue: BTreeMap<TaskDescriptor, Box<SchedulableTask>>,
    pub(super) running_task: Option<Box<SchedulableTask>>,
}

impl RunQueue {
    pub const fn new() -> Self {
        Self {
            total_weight: 0,
            queue: BTreeMap::new(),
            running_task: None,
        }
    }

    pub fn switch_tasks(&mut self, next_task: TaskDescriptor, now_inst: Instant) -> SwitchResult {
        if let Some(current) = self.current()
            && current.descriptor() == next_task
        {
            return SwitchResult::AlreadyRunning;
        }

        let mut new_task = match self.queue.remove(&next_task) {
            Some(t) => t,
            None => {
                warn!("Task {next_task:?} not found for switch.");
                return SwitchResult::AlreadyRunning;
            }
        };

        new_task.about_to_execute(now_inst);

        // Perform the swap.
        if let Some(old_task) = self.running_task.replace(new_task) {
            let state = *old_task.state.lock_save_irq();

            match state {
                TaskState::Running | TaskState::Runnable => {
                    // Update state to strictly Runnable
                    *old_task.state.lock_save_irq() = TaskState::Runnable;

                    self.queue.insert(old_task.descriptor(), old_task);

                    return SwitchResult::Preempted;
                }
                _ => {
                    self.total_weight = self.total_weight.saturating_sub(old_task.weight() as u64);

                    return SwitchResult::Blocked { old_task };
                }
            }
        }

        // If there was no previous task (e.g., boot up), it counts as a
        // Preemption.
        SwitchResult::Preempted
    }

    pub fn weight(&self) -> u64 {
        self.total_weight
    }

    #[allow(clippy::borrowed_box)]
    pub fn current(&self) -> Option<&Box<SchedulableTask>> {
        self.running_task.as_ref()
    }

    pub fn current_mut(&mut self) -> Option<&mut Box<SchedulableTask>> {
        self.running_task.as_mut()
    }

    fn fallback_current_or_idle(&self) -> TaskDescriptor {
        if let Some(ref current) = self.running_task {
            let s = *current.state.lock_save_irq();
            if !current.is_idle_task() && (s == TaskState::Runnable || s == TaskState::Running) {
                return current.descriptor();
            }
        }

        TaskDescriptor::this_cpus_idle()
    }

    /// Returns the Descriptor of the best task to run next. This compares the
    /// best task in the run_queue against the currently running task.
    pub fn find_next_runnable_desc(&self, vclock: u128) -> TaskDescriptor {
        // Find the best candidate from the Run Queue
        let best_queued_entry = self
            .queue
            .iter()
            .filter(|(_, task)| {
                !task.is_idle_task() && task.v_eligible.saturating_sub(vclock) <= VCLOCK_EPSILON
            })
            .min_by(|(_, t1), (_, t2)| t1.compare_with(t2));

        let (best_queued_desc, best_queued_task) = match best_queued_entry {
            Some((d, t)) => (*d, t),
            // If runqueue is empty (or no eligible tasks), we might just run
            // current or idle.
            None => return self.fallback_current_or_idle(),
        };

        // Compare against the current task
        if let Some(current) = self.current() {
            // If current is not Runnable (e.g. it blocked, yielded, or
            // finished), it cannot win.
            let current_state = *current.state.lock_save_irq();
            if current_state != TaskState::Runnable && current_state != TaskState::Running {
                return best_queued_desc;
            }

            // compare current vs challenger
            match current.compare_with(best_queued_task) {
                Ordering::Less | Ordering::Equal => {
                    // Current is better (has earlier deadline) or equal. Keep
                    // running current.
                    return current.descriptor();
                }
                Ordering::Greater => {
                    // Queued task is better. Switch.
                    return best_queued_desc;
                }
            }
        }

        best_queued_desc
    }

    /// Inserts `task` into this CPU's run-queue.
    pub fn enqueue_task(&mut self, new_task: Box<SchedulableTask>) {
        if !new_task.is_idle_task() {
            self.total_weight = self.total_weight.saturating_add(new_task.weight() as u64);
        }

        if let Some(old_task) = self.queue.insert(new_task.descriptor(), new_task) {
            // Handle the edge case where we overwrite a task. If we replaced
            // someone, we must subtract their weight to avoid accounting drift.
            warn!("Overwrote active task {:?}", old_task.descriptor());
            self.total_weight = self.total_weight.saturating_sub(old_task.weight() as u64);
        }
    }
}
