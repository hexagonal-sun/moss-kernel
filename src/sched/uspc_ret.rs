use super::{SCHED_STATE, current_task, schedule, waker::create_waker};
use crate::process::TASK_LIST;
use crate::{
    arch::{Arch, ArchImpl},
    process::{
        TaskState,
        ctx::UserCtx,
        exit::kernel_exit_with_signal,
        thread_group::{
            signal::{SigId, ksigaction::KSignalAction},
            wait::ChildState,
        },
    },
};
use alloc::boxed::Box;
use core::{ptr, task::Poll};

enum State {
    PickNewTask,
    ProcessKernelWork,
    ReturnToUserspace,
}

/// Prepares the kernel for a safe return to userspace, guaranteeing a valid
/// context frame is prepared.
///
/// This function is the primary gateway for transitioning from a kernel context
/// (e.g., after a syscall, interrupt, or fault) back to userspace code. It
/// ensures a task is in a valid state to resume execution, handling any
/// necessary kernel business before the final context restore.
///
/// The core of this function is a loop that guarantees progress. It will always
/// find a runnable task and prepare its context for userspace return, even if
/// the initial task needs to be terminated, put to sleep, or handle a signal
/// first.
///
/// The logical flow inside the loop for a chosen task is as follows:
///
/// 1. Schedule: A task is selected to run next. This may trigger a context
///    switch if the scheduler picks a different task than the one that entered
///    the kernel.
/// 2. Poll In-Flight Work: It checks for and polls any outstanding asynchronous
///    work associated with the task, such as signal delivery (`signal_work`) or
///    another kernel future (`kern_work`).
///     - If the work is `Poll::Pending`, the task is put to sleep, and the loop
///       continues to find another task.
///     - If the work completes successfully (`Poll::Ready(Ok(_))`), the
///       function proceeds.
///     - If signal delivery fails (`Poll::Ready(Err(_))`), it's a fatal error,
///       and the task is terminated. The loop continues to find another task.
/// 3. Action New Signals: If there is no in-flight work, it checks for any new
///    pending signals. Depending on the signal's disposition, the task may be
///    terminated or new `signal_work` may be queued. If new work is queued, the
///    loop restarts to process it immediately.
/// 4. Final Return: If a task has no pending work and no new signals to action,
///    its userspace context is restored into the `ctx` pointer, the current
///    address space is guaranteed to be active, and the function returns.
///
/// # Parameters
/// - `ctx`: A mutable pointer to the stack-allocated `UserCtx` (the exception
///   frame). This function will write the register context of the task being
///   returned to into this location, making it ready for the
///   architecture-specific return-to-userspace instructions.
///
/// # Guarantees
/// - This function will always return; it does not diverge (unless it panics).
/// - Upon return, the address space of the task being resumed will be active.
/// - Upon return, the memory pointed to by `ctx` will be populated with the
///   valid userspace register state for that task.
///
/// # Panics
/// This function will panic if it detects an attempt to process signals or
/// kernel work for the special idle task, as this indicates a critical bug in
/// the scheduler or task management.
pub fn dispatch_userspace_task(ctx: *mut UserCtx) {
    let mut state = State::PickNewTask;

    loop {
        match state {
            State::PickNewTask => {
                // Pick a new task, potentially context switching to a new task.
                schedule();
                state = State::ProcessKernelWork;
            }
            State::ProcessKernelWork => {
                let task = current_task();

                // First, let's handle signals. If there is any scheduled signal
                // work (this has to be async to handle faults, etc).
                let signal_work = task.ctx.lock_save_irq().take_signal_work();
                if let Some(mut signal_work) = signal_work {
                    if task.is_idle_task() {
                        panic!("Signal processing for idle task");
                    }

                    match signal_work
                        .as_mut()
                        .poll(&mut core::task::Context::from_waker(&create_waker(
                            task.descriptor(),
                        ))) {
                        Poll::Ready(Ok(state)) => {
                            // Signal actioning is complete. Return to userspace.
                            unsafe { ptr::copy_nonoverlapping(&state as _, ctx, 1) };
                            return;
                        }
                        Poll::Ready(Err(_)) => {
                            // If we errored, then we *cannot* progress the task.
                            // Delivery of the signal failed. Force the process to
                            // terminate.
                            kernel_exit_with_signal(SigId::SIGSEGV, true);

                            // Look for another task, this one is now dead.
                            state = State::PickNewTask;
                            continue;
                        }
                        Poll::Pending => {
                            task.ctx.lock_save_irq().put_signal_work(signal_work);
                            let mut task_state = task.state.lock_save_irq();

                            match *task_state {
                                // The main path we expect to take to sleep the
                                // task.
                                // Task is currently running or is runnable and will now sleep.
                                TaskState::Running | TaskState::Runnable => {
                                    *task_state = TaskState::Sleeping
                                }
                                // If we were woken between the future returning
                                // `Poll::Pending` and acquiring the lock above,
                                // the waker will have put us into this state.
                                // Transition back to `Running` since we're
                                // ready to progress with more work.
                                TaskState::Woken => *task_state = TaskState::Running,
                                // We should never get here for any other state.
                                s => {
                                    unreachable!(
                                        "Unexpected task state {s:?} during signal task sleep"
                                    );
                                }
                            }

                            state = State::PickNewTask;
                            continue;
                        }
                    }
                }

                // Now let's handle any kernel work that's been spawned for this task.
                let kern_work = task.ctx.lock_save_irq().take_kernel_work();
                if let Some(mut kern_work) = kern_work {
                    if task.is_idle_task() {
                        panic!("Idle process should never have kernel work");
                    }

                    match kern_work
                        .as_mut()
                        .poll(&mut core::task::Context::from_waker(&create_waker(
                            task.descriptor(),
                        ))) {
                        Poll::Ready(()) => {
                            // If the task just exited (entered the finished state),
                            // don't return to it's userspace, instead, find another
                            // task to execute, removing this task from the
                            // runqueue, reaping it's resouces.
                            if task.state.lock_save_irq().is_finished() {
                                SCHED_STATE
                                    .borrow_mut()
                                    .remove_task_with_weight(&task.descriptor());
                                let mut task_list = TASK_LIST.lock_save_irq();
                                task_list.remove(&task.descriptor());

                                state = State::PickNewTask;
                                continue;
                            }

                            // Kernel work finished. Ensure we have no other new
                            // work to process (i.e. a signal was rasied). We
                            // don't need to clear the kernel context here as we
                            // used the *take* function above.
                            state = State::ProcessKernelWork;
                            continue;
                        }
                        Poll::Pending => {
                            // Kernel work hasn't finished. A wake up should
                            // have been scheduled by the future. Replace the
                            // kernel work context back into the task, set it's
                            // state to sleeping so it's not scheduled again and
                            // search for another task to execute.
                            task.ctx.lock_save_irq().put_kernel_work(kern_work);
                            let mut task_state = task.state.lock_save_irq();

                            match *task_state {
                                // Task is runnable or running, put it to sleep.
                                TaskState::Running | TaskState::Runnable => {
                                    *task_state = TaskState::Sleeping
                                }
                                // If we were woken between the future returning
                                // `Poll::Pending` and acquiring the lock above,
                                // the waker will have put us into this state.
                                // Transition back to `Running` since we're
                                // ready to progress with more work.
                                TaskState::Woken => *task_state = TaskState::Running,
                                // We should never get here for any other state.
                                s => {
                                    unreachable!(
                                        "Unexpected task state {s:?} during kernel task sleep"
                                    );
                                }
                            }
                            state = State::PickNewTask;
                            continue;
                        }
                    }
                }

                // No kernel work. Check for any pending signals.

                // We never handle signals for the idle task.
                if task.is_idle_task() {
                    state = State::ReturnToUserspace;
                    continue;
                }

                // See if there are any signals we need to action.
                let task = current_task();
                let mut pending_task_sigs = task.pending_signals.lock_save_irq();
                let mask = task.sig_mask.lock_save_irq();
                if let Some((id, action)) = task
                    .process
                    .signals
                    .lock_save_irq()
                    .action_signal(*mask, &mut pending_task_sigs)
                {
                    match action {
                        KSignalAction::Term | KSignalAction::Core => {
                            // Terminate the process, and find a new task.
                            kernel_exit_with_signal(id, false);

                            state = State::PickNewTask;
                            continue;
                        }
                        KSignalAction::Stop => {
                            // Default action: stop (suspend) the entire process.
                            let process = &task.process;

                            // Notify the parent that we have stopped (SIGCHLD).
                            if let Some(parent) = process
                                .parent
                                .lock_save_irq()
                                .as_ref()
                                .and_then(|p| p.upgrade())
                            {
                                parent
                                    .child_notifiers
                                    .child_update(process.tgid, ChildState::Stop { signal: id });
                                parent.signals.lock_save_irq().set_pending(SigId::SIGCHLD);
                            }

                            for thr_weak in process.tasks.lock_save_irq().values() {
                                if let Some(thr) = thr_weak.upgrade() {
                                    *thr.state.lock_save_irq() = TaskState::Stopped;
                                }
                            }

                            state = State::PickNewTask;
                            continue;
                        }
                        KSignalAction::Continue => {
                            let process = &task.process;

                            // Wake up all sleeping threads in the process.
                            for thr_weak in process.tasks.lock_save_irq().values() {
                                if let Some(thr) = thr_weak.upgrade() {
                                    let mut st = thr.state.lock_save_irq();
                                    if *st == TaskState::Sleeping {
                                        *st = TaskState::Runnable;
                                    }
                                }
                            }

                            // Notify the parent that we have continued (SIGCHLD).
                            if let Some(parent) = process
                                .parent
                                .lock_save_irq()
                                .as_ref()
                                .and_then(|p| p.upgrade())
                            {
                                parent
                                    .child_notifiers
                                    .child_update(process.tgid, ChildState::Continue);
                                parent.signals.lock_save_irq().set_pending(SigId::SIGCHLD);
                            }

                            // Re-process kernel work for this task (there may be more to do).
                            state = State::ProcessKernelWork;
                            continue;
                        }
                        KSignalAction::Userspace(id, action) => {
                            let fut = ArchImpl::do_signal(id, action);

                            task.ctx.lock_save_irq().put_signal_work(Box::pin(fut));

                            state = State::ProcessKernelWork;
                            continue;
                        }
                    }
                }

                state = State::ReturnToUserspace;
            }

            State::ReturnToUserspace => {
                // Real user-space return now.
                current_task().ctx.lock_save_irq().restore_user_ctx(ctx);
                return;
            }
        }
    }
}
