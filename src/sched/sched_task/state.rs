use atomic_enum::atomic_enum;
use core::{fmt::Display, sync::atomic::Ordering};

#[atomic_enum]
#[derive(PartialEq, Eq)]
pub enum TaskState {
    Running,
    Runnable,
    Woken,
    Stopped,
    Sleeping,
    Finished,
    /// Wants to sleep but RunnableTask still on the runqueue.
    PendingSleep,
    /// Wants to stop but RunnableTask still on the runqueue;
    PendingStop,
}

impl Display for TaskState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state_str = match self {
            TaskState::Running => "R",
            TaskState::Runnable => "R",
            TaskState::Woken => "W",
            TaskState::Stopped => "T",
            TaskState::Sleeping => "S",
            TaskState::Finished => "Z",
            TaskState::PendingSleep => "S",
            TaskState::PendingStop => "T",
        };
        write!(f, "{state_str}")
    }
}

impl TaskState {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Finished)
    }
}

/// What the waker should do after calling `wake()`.
pub enum WakerAction {
    /// Task was Sleeping/Stopped -> Runnable. Safe to call add_work.
    Enqueue,
    /// Task was Running/Pending* -> Woken. Sleep prevented, no enqueue needed.
    PreventedSleep,
    /// No action needed (already Woken/Finished, or CAS lost to another CPU).
    None,
}

/// State machine wrapper around `AtomicTaskState`.
///
/// Only exposes named transition methods to enforce state transition logic.
///
/// ```text
///                              new()
///                                |
///                                v
///    +------- wake() ------> RUNNABLE <------- wake() -------+-------+
///    |      (Enqueue)            |            (Enqueue)      |       |
///    |                      activate()                       |       |
///    |                           |                           |       |
///    |                           v                           |       |
///    |                       RUNNING ----> finish() ---> FINISHED    |
///    |                       /      \                                |
///    |      try_pending     /        \   try_pending                 |
///    |      _sleep()       /          \                              |
///    |                    v            v                             |
///    |            PENDING_SLEEP    PENDING_STOP                      |
///    |                    |            |                             |
///    |       finalize     |            |    finalize                 |
///    |       _deact()     |            |    _deact()                 |
///    |                    v            v                             |
///    +--------------- SLEEPING     STOPPED --------------------------+
///
///    Race: wake() on {Running, Runnable, PendingSleep, PendingStop} --> WOKEN.
///    try_pending_sleep() clears WOKEN back to Running (prevents spurious
///    sleep).
/// ```
pub struct TaskStateMachine(AtomicTaskState);

#[cfg(test)]
mod tests {
    use super::*;
    use moss_macros::ktest;
    #[ktest]
    fn finished_tasks_cannot_be_reactivated_or_enqueued() {
        let state = TaskStateMachine::new();
        state.finish();
        assert!(!state.activate());
        state.mark_runnable();
        assert_eq!(state.load(Ordering::Acquire), TaskState::Finished);
        assert!(matches!(state.wake(), WakerAction::None));
        assert!(state.try_pending_sleep());
    }
}

impl TaskStateMachine {
    /// New tasks starts as Runnable.
    pub fn new() -> Self {
        Self(AtomicTaskState::new(TaskState::Runnable))
    }

    /// Read the current state.
    pub fn load(&self, ordering: Ordering) -> TaskState {
        self.0.load(ordering)
    }

    /// Scheduler is about to execute this task (-> Running).
    pub fn activate(&self) -> bool {
        self.transition_unless_finished(TaskState::Running)
    }

    fn transition_unless_finished(&self, next: TaskState) -> bool {
        loop {
            let state = self.load(Ordering::Acquire);
            if state == TaskState::Finished {
                return false;
            }
            if self
                .0
                .compare_exchange(state, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Task placed on a run queue (-> Runnable).
    pub fn mark_runnable(&self) {
        self.transition_unless_finished(TaskState::Runnable);
    }

    /// Scheduler finalizes deactivation after dropping RunnableTask.
    ///
    /// CAS PendingSleep -> Sleeping or PendingStop -> Stopped. Returns `true`
    /// if deactivated, `false` if woken (caller should re-enqueue).
    pub fn finalize_deactivation(&self) -> bool {
        let state = self.0.load(Ordering::Acquire);
        let target = match state {
            TaskState::PendingSleep => TaskState::Sleeping,
            TaskState::PendingStop => TaskState::Stopped,
            // Already woken concurrently.
            _ => return false,
        };

        self.0
            .compare_exchange(state, target, Ordering::Release, Ordering::Acquire)
            .is_ok()
    }

    /// Future returned `Poll::Pending`, try to initiate sleep.
    ///
    /// CAS Running -> PendingSleep. Returns `true` if sleep initiated and a new
    /// task should be scheduled, `false` if woken/finished and we should
    /// re-poll.
    pub fn try_pending_sleep(&self) -> bool {
        let state = self.0.load(Ordering::Acquire);
        match state {
            TaskState::Running | TaskState::Runnable => {
                match self.0.compare_exchange(
                    state,
                    TaskState::PendingSleep,
                    Ordering::Release,
                    Ordering::Acquire,
                ) {
                    Ok(_) => true,
                    Err(TaskState::Woken) => {
                        // Woken between load and CAS — clear woken, don't sleep.
                        self.0
                            .compare_exchange(
                                TaskState::Woken,
                                TaskState::Running,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_err_and(|s| s == TaskState::Finished)
                    }
                    Err(TaskState::Finished) => true,
                    Err(s) => {
                        unreachable!("Unexpected task state {s:?} during pending_sleep transition")
                    }
                }
            }
            TaskState::Woken => self
                .0
                .compare_exchange(
                    TaskState::Woken,
                    TaskState::Running,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err_and(|s| s == TaskState::Finished),
            TaskState::Finished => true,
            s => unreachable!("Unexpected task state {s:?} during pending_sleep transition"),
        }
    }

    /// Ptrace/signal stop: try CAS Running -> PendingStop.
    ///
    /// Returns `true` if the stop was initiated, `false` if the task was woken
    /// concurrently (caller should re-process kernel work instead of sleeping).
    pub fn try_pending_stop(&self) -> bool {
        let state = self.0.load(Ordering::Acquire);
        match state {
            TaskState::Running | TaskState::Runnable => self
                .0
                .compare_exchange(
                    state,
                    TaskState::PendingStop,
                    Ordering::Release,
                    Ordering::Acquire,
                )
                .is_ok(),
            TaskState::Woken => {
                self.0.store(TaskState::Running, Ordering::Release);
                false
            }
            // Already stopped/sleeping/finished — nothing to do.
            _ => true,
        }
    }

    /// Wake the task. Returns what the caller should do.
    pub fn wake(&self) -> WakerAction {
        loop {
            let state = self.0.load(Ordering::Acquire);
            match state {
                TaskState::Sleeping | TaskState::Stopped => {
                    if self
                        .0
                        .compare_exchange(
                            state,
                            TaskState::Runnable,
                            Ordering::Release,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        return WakerAction::Enqueue;
                    }
                    // Another CPU handled it.
                    return WakerAction::None;
                }
                TaskState::Running
                | TaskState::Runnable
                | TaskState::PendingSleep
                | TaskState::PendingStop => {
                    // Task hasn't fully deactivated yet. Set Woken to prevent
                    // the sleep/stop transition from completing.
                    if self
                        .0
                        .compare_exchange(
                            state,
                            TaskState::Woken,
                            Ordering::Release,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        return WakerAction::PreventedSleep;
                    }
                    // State changed under us (e.g. PendingSleep -> Sleeping).
                    // Retry.
                    continue;
                }
                // Already Woken or Finished: noop.
                _ => return WakerAction::None,
            }
        }
    }

    /// Mark task as finished (-> Finished).
    pub fn finish(&self) {
        self.0.store(TaskState::Finished, Ordering::Release);
    }

    /// Check if finished.
    pub fn is_finished(&self) -> bool {
        self.0.load(Ordering::Acquire).is_finished()
    }
}
