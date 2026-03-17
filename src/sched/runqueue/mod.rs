use super::{
    NUM_CONTEXT_SWITCHES,
    sched_task::{RunnableTask, Work, state::TaskState},
};
use crate::{
    arch::{Arch, ArchImpl},
    drivers::timer::Instant,
};
use alloc::{boxed::Box, collections::binary_heap::BinaryHeap, sync::Arc, vec::Vec};
use core::{cmp, ptr, sync::atomic::Ordering};
use libkernel::CpuOps;
use vclock::VClock;

mod vclock;

// Wrapper for the Ineligible Heap (Min-Heap ordered by v_eligible)
struct ByEligible(RunnableTask);

impl PartialEq for ByEligible {
    fn eq(&self, other: &Self) -> bool {
        self.0.v_eligible == other.0.v_eligible
    }
}

impl Eq for ByEligible {}

impl Ord for ByEligible {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        other.0.v_eligible.cmp(&self.0.v_eligible)
    }
}

impl PartialOrd for ByEligible {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// Wrapper for the Eligible Heap (Min-Heap ordered by deadline)
struct ByDeadline(RunnableTask);

impl PartialEq for ByDeadline {
    fn eq(&self, other: &Self) -> bool {
        other.0.compare_with(&self.0) == cmp::Ordering::Equal
    }
}

impl Eq for ByDeadline {}

impl Ord for ByDeadline {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        // Reversed so BinaryHeap acts as a MIN-heap
        other.0.compare_with(&self.0)
    }
}

impl PartialOrd for ByDeadline {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A simple weight-tracking runqueue.
///
/// Invariants:
/// 1. `total_weight` = Sum(queue tasks) + Weight(running_task) (excluding the idle task).
/// 2. `running_task` is NOT in `queue`.
pub struct RunQueue {
    total_weight: u64,
    ineligible: BinaryHeap<ByEligible>,
    eligible: BinaryHeap<ByDeadline>,
    pub(super) running_task: Option<RunnableTask>,
    v_clock: VClock,
    idle: RunnableTask,
}

impl RunQueue {
    pub fn new() -> Self {
        let idle = Work::new(Box::new(ArchImpl::create_idle_task())).into_runnable();

        Self {
            total_weight: 0,
            ineligible: BinaryHeap::new(),
            eligible: BinaryHeap::new(),
            running_task: None,
            v_clock: VClock::new(),
            idle,
        }
    }

    /// Picks the next task to execute and performs the context switch.
    ///
    /// Returns `RunnableTask`s that must be dropped after the caller releases
    /// `SCHED_STATE`. Dropping inside the borrow can trigger waker calls that
    /// re-enter `SCHED_STATE` and panic.
    pub fn schedule(&mut self, now: Instant) -> Vec<RunnableTask> {
        self.v_clock.advance(now, self.weight());

        let mut prev_task = ptr::null();
        let mut next_task = None;
        let mut deferred_drops: Vec<RunnableTask> = Vec::new();

        if let Some(mut cur_task) = self.running_task.take() {
            prev_task = Arc::as_ptr(&cur_task.work);
            let state = cur_task.work.state.load(Ordering::Acquire);
            match state {
                TaskState::Running | TaskState::Woken => {
                    if cur_task.tick(now) {
                        // Deadline exceeded — requeue for the next time slice.
                        self.enqueue(cur_task);
                    } else {
                        // Still has budget — keep running.
                        next_task = Some(cur_task);
                    }
                }
                TaskState::PendingSleep | TaskState::PendingStop => {
                    // Task wants to deactivate. Drop the RunnableTask now to
                    // restore sched_data.
                    let work = cur_task.work.clone();
                    cur_task.sched_data.last_cpu = ArchImpl::id();
                    self.total_weight = self.total_weight.saturating_sub(cur_task.weight() as u64);
                    drop(cur_task);

                    if !work.state.finalize_deactivation() {
                        // Woken concurrently — re-enqueue.
                        self.add_work(work);
                    }
                }
                _ => {
                    // Finished — remove weight. Defer the drop.
                    self.total_weight = self.total_weight.saturating_sub(cur_task.weight() as u64);
                    deferred_drops.push(cur_task);
                }
            }
        }

        if let Some(mut next_task) = next_task.or_else(|| self.find_next_task(&mut deferred_drops))
        {
            next_task.about_to_execute(now);

            if Arc::as_ptr(&next_task.work) != prev_task {
                // If we scheduled a different task than before, context switch.
                NUM_CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);

                next_task.switch_context();

                next_task.work.reset_last_account(now);
            }

            self.running_task = Some(next_task);
        } else {
            // No next task.  Go idle.
            self.idle.switch_context();
        }

        deferred_drops
    }

    /// Pops any tasks that were ineligible which have become eligible from the
    /// ineligible queue.
    fn pop_now_eligible_task(&mut self) -> Option<RunnableTask> {
        let ByEligible(tsk) = self.ineligible.peek()?;

        if self.v_clock.is_task_eligible(tsk) {
            let ByEligible(tsk) = self.ineligible.pop()?;

            Some(tsk)
        } else {
            None
        }
    }

    /// Returns the best task to run next, skipping any Finished tasks.
    ///
    /// Finished tasks found in the heaps are removed and pushed into
    /// `deferred_drops` so they are dropped outside the `SCHED_STATE` borrow.
    ///
    /// # Returns
    /// - `None` when no runnable task can be found (the runqueue is empty).
    /// - `Some(tsk)` when the current task should be replaced with `tsk`.
    fn find_next_task(&mut self, deferred_drops: &mut Vec<RunnableTask>) -> Option<RunnableTask> {
        while let Some(tsk) = self.pop_now_eligible_task() {
            self.eligible.push(ByDeadline(tsk));
        }

        while let Some(ByDeadline(best)) = self.eligible.pop() {
            if best.work.state.load(Ordering::Acquire).is_finished() {
                self.total_weight = self.total_weight.saturating_sub(best.weight() as u64);
                deferred_drops.push(best);
                continue;
            }
            return Some(best);
        }

        // Fast forward logic, if we have non-eligible, don't go idle.
        // Fast-forward vclk to the next earliest `v_eligible`.
        if let Some(ByEligible(tsk)) = self.ineligible.peek() {
            self.v_clock.fast_forward(tsk.v_eligible);
            return self.find_next_task(deferred_drops);
        }

        // The runqueues are completely empty.  Go idle.
        None
    }

    fn enqueue(&mut self, mut task: RunnableTask) {
        task.refresh_priority();
        task.work.state.mark_runnable();

        if self.v_clock.is_task_eligible(&task) {
            self.eligible.push(ByDeadline(task));
        } else {
            self.ineligible.push(ByEligible(task));
        }
    }

    /// Inserts `new_task` into this CPU's run-queue.
    pub fn add_work(&mut self, new_task: Arc<Work>) {
        let mut new_task = new_task.into_runnable();

        new_task.inserting_into_runqueue(self.v_clock.now());

        self.total_weight = self.total_weight.saturating_add(new_task.weight() as u64);

        self.enqueue(new_task);
    }

    pub fn weight(&self) -> u64 {
        self.total_weight
    }

    #[allow(clippy::borrowed_box)]
    pub fn current(&self) -> &RunnableTask {
        self.running_task.as_ref().unwrap_or(&self.idle)
    }

    pub fn current_mut(&mut self) -> &mut RunnableTask {
        self.running_task.as_mut().unwrap_or(&mut self.idle)
    }
}
