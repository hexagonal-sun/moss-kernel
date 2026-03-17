use super::{current_work, current_work_waker, schedule};
use crate::{
    arch::{Arch, ArchImpl},
    process::{
        ctx::UserCtx,
        exit::kernel_exit_with_signal,
        thread_group::{
            signal::{SigId, ksigaction::KSignalAction},
            wait::ChildState,
        },
    },
    sched::syscall_ctx::ProcessCtx,
};
use alloc::boxed::Box;
use core::{ptr, sync::atomic::Ordering, task::Poll};

enum State {
    PickNewTask,
    ProcessKernelWork,
    ReturnToUserspace,
}

/// Try to transition the current task from Running to PendingSleep atomically.
///
/// Returns `true` if the task should go to sleep (state set to PendingSleep or
/// task is Finished). Returns `false` if the task was woken concurrently
/// and should re-process its kernel work instead.
fn try_sleep_current() -> bool {
    current_work().state.try_pending_sleep()
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
pub fn dispatch_userspace_task(frame: *mut UserCtx) {
    let mut state = State::PickNewTask;

    // SAFETY: Access is exclusive since we're not polling any futures.
    let mut ctx = unsafe { ProcessCtx::from_current() };

    'dispatch: loop {
        match state {
            State::PickNewTask => {
                // Pick a new task, potentially context switching to a new task.
                schedule();
                // SAFETY: As above.
                ctx = unsafe { ProcessCtx::from_current() };
                state = State::ProcessKernelWork;
            }
            State::ProcessKernelWork => {
                // First, let's handle signals. If there is any scheduled signal
                // work (this has to be async to handle faults, etc).
                let signal_work = ctx.task_mut().ctx.take_signal_work();
                let is_idle = ctx.task().is_idle_task();

                if let Some(mut signal_work) = signal_work {
                    if is_idle {
                        panic!("Signal processing for idle task");
                    }

                    match signal_work
                        .as_mut()
                        .poll(&mut core::task::Context::from_waker(&current_work_waker()))
                    {
                        Poll::Ready(Ok(restored)) => {
                            // Signal actioning is complete. Return to userspace.
                            unsafe { ptr::copy_nonoverlapping(&restored as _, frame, 1) };
                            return;
                        }
                        Poll::Ready(Err(_)) => {
                            // If we errored, then we *cannot* progress the task.
                            // Delivery of the signal failed. Force the process to
                            // terminate.
                            kernel_exit_with_signal(ctx.shared().clone(), SigId::SIGSEGV, true);

                            // Look for another task, this one is now dead.
                            state = State::PickNewTask;
                            continue;
                        }
                        Poll::Pending => {
                            ctx.task_mut().ctx.put_signal_work(signal_work);

                            if try_sleep_current() {
                                state = State::PickNewTask;
                            } else {
                                state = State::ProcessKernelWork;
                            }
                            continue;
                        }
                    }
                }

                // Now let's handle any kernel work that's been spawned for this task.
                let kern_work = ctx.task_mut().ctx.take_kernel_work();
                if let Some(mut kern_work) = kern_work {
                    if is_idle {
                        panic!("Idle process should never have kernel work");
                    }

                    match kern_work
                        .as_mut()
                        .poll(&mut core::task::Context::from_waker(&current_work_waker()))
                    {
                        Poll::Ready(()) => {
                            // If the task just exited (entered the finished
                            // state), don't return to it's userspace, instead,
                            // find another task to execute, removing this task
                            // from the runqueue, reaping it's resouces.
                            if current_work().state.load(Ordering::Acquire).is_finished() {
                                state = State::PickNewTask;
                                continue;
                            }

                            // Kernel work finished. Ensure we have no other new
                            // work to process (i.e. a signal was rasied, trace
                            // point was hit). We don't need to clear the kernel
                            // context here as we used the *take* function
                            // above.
                            state = State::ProcessKernelWork;
                            continue;
                        }
                        Poll::Pending => {
                            ctx.task_mut().ctx.put_kernel_work(kern_work);

                            if try_sleep_current() {
                                state = State::PickNewTask;
                            } else {
                                state = State::ProcessKernelWork;
                            }
                            continue;
                        }
                    }
                }

                // No kernel work. Check for any pending signals.

                // We never handle signals for the idle task.
                if ctx.task().is_idle_task() {
                    state = State::ReturnToUserspace;
                    continue;
                }

                while let Some(signal) = ctx.task().take_signal() {
                    let mut ptrace = ctx.task().ptrace.lock_save_irq();
                    if ptrace.trace_signal(signal, ctx.task().ctx.user()) {
                        ptrace.set_waker(current_work_waker());
                        ptrace.notify_tracer_of_trap(&ctx.task().process);
                        drop(ptrace);

                        if current_work().state.try_pending_stop() {
                            state = State::PickNewTask;
                        } else {
                            // Woken concurrently (tracer already resumed us).
                            state = State::ProcessKernelWork;
                        }
                        continue 'dispatch;
                    }
                    drop(ptrace);

                    let sigaction = ctx
                        .task()
                        .process
                        .signals
                        .lock_save_irq()
                        .action_signal(signal);

                    match sigaction {
                        // Signal ignored, look for another.
                        None => continue,
                        Some(KSignalAction::Term | KSignalAction::Core) => {
                            // Terminate the process, and find a new task.
                            kernel_exit_with_signal(ctx.shared().clone(), signal, false);

                            state = State::PickNewTask;
                            continue 'dispatch;
                        }
                        Some(KSignalAction::Stop) => {
                            // Default action: stop (suspend) the entire process.
                            let process = &ctx.task().process;

                            // Notify the parent that we have stopped (SIGCHLD).
                            if let Some(parent) = process
                                .parent
                                .lock_save_irq()
                                .as_ref()
                                .and_then(|p| p.upgrade())
                            {
                                parent
                                    .child_notifiers
                                    .child_update(process.tgid, ChildState::Stop { signal });

                                parent.deliver_signal(SigId::SIGCHLD);
                            }

                            for thr_weak in process.tasks.lock_save_irq().values() {
                                if let Some(thr) = thr_weak.upgrade() {
                                    thr.state.try_pending_stop();
                                }
                            }

                            state = State::PickNewTask;
                            continue 'dispatch;
                        }
                        Some(KSignalAction::Continue) => {
                            let process = &ctx.task().process;

                            // Wake up all stopped/sleeping threads in the process.
                            for thr_weak in process.tasks.lock_save_irq().values() {
                                if let Some(thr) = thr_weak.upgrade() {
                                    crate::sched::waker::create_waker(thr).wake();
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

                                parent.deliver_signal(SigId::SIGCHLD);
                            }

                            // Re-process kernel work for this task (there may be more to do).
                            state = State::ProcessKernelWork;
                            continue 'dispatch;
                        }
                        Some(KSignalAction::Userspace(id, action)) => {
                            // SAFETY: Signal work will be polled indepdently of
                            // kernel work. Therefore there will be no
                            // concurrent accesses of the ctx.
                            let ctx2 = unsafe { ctx.clone() };
                            let fut = ArchImpl::do_signal(ctx2, id, action);

                            ctx.task_mut().ctx.put_signal_work(Box::pin(fut));

                            state = State::ProcessKernelWork;
                            continue 'dispatch;
                        }
                    }
                }

                state = State::ReturnToUserspace;
            }

            State::ReturnToUserspace => {
                // Real user-space return now.
                ctx.task().ctx.restore_user_ctx(frame);
                return;
            }
        }
    }
}
