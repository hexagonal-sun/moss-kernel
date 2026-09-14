use super::{
    TASK_LIST, Task,
    ptrace::{TracePoint, ptrace_stop},
    thread_group::{ProcessState, Tgid, ThreadGroup, signal::SigId, wait::ChildState},
    threading::futex::{self, key::FutexKey},
};
use crate::clock::syscalls::itimer::cleanup_itimers;
use crate::memory::uaccess::copy_to_user;
use crate::sched::syscall_ctx::ProcessCtx;
use crate::sched::{self};
use alloc::vec::Vec;
use libkernel::error::Result;
use log::warn;
use ringbuf::Arc;

pub async fn do_exit_group(task: &Arc<Task>, exit_code: ChildState) {
    let process = Arc::clone(&task.process);

    if process.tgid.is_init() {
        panic!("Attempted to kill init");
    }

    {
        let _pid_op = super::pid_namespace::PID_OPS.lock_save_irq();
        let mut process_state = process.state.lock_save_irq();

        // Check if we're already exiting (e.g., two threads call exit_group at
        // once)
        if *process_state != ProcessState::Running {
            // We're already on our way out. Just kill this thread.
            drop(process_state);
            sched::current_work().state.finish();
            return;
        }

        // It's our job to tear it all down. Mark the process as exiting.
        *process_state = ProcessState::Exiting;
    }

    let ns = process.pid.namespace();
    let namespace_init = ns.level != 0 && process.pid.local() == 1;
    if namespace_init {
        ns.disable();
        process.child_notifiers.begin_shutdown();
    }

    // Signal all other threads in the group to terminate. We iterate over Weak
    // pointers and upgrade them.
    for thread_weak in process.tasks.lock_save_irq().values() {
        if let Some(other_thread) = thread_weak.upgrade() {
            cleanup_itimers(&other_thread);
            // Don't signal ourselves
            if other_thread.tid != task.tid {
                // TODO: Send an IPI/Signal to halt execution now. For now, just
                // wait for the scheduler to never schedule any of it's tasks
                // again.
                other_thread.state.finish();
            }
        }
    }

    // A Finished state alone is not an SMP execution barrier: the other CPU
    // must release its RunnableTask before we notify a waiter or vfork parent.
    loop {
        let peers: Vec<_> = process
            .tasks
            .lock_save_irq()
            .values()
            .filter_map(|w| w.upgrade())
            .filter(|w| w.tid != task.tid)
            .collect();
        if peers.iter().all(|w| w.sched_data.lock_save_irq().is_some()) {
            break;
        }
        drop(peers);
        crate::drivers::timer::sleep(core::time::Duration::from_millis(1)).await;
    }
    if namespace_init {
        // Allocation/publication is disabled before collecting descendants.
        // Include processes parented outside this namespace through setns.
        loop {
            let groups: Vec<_> = super::thread_group::TG_LIST
                .lock_save_irq()
                .values()
                .filter_map(|w| w.upgrade())
                .filter(|p| p.tgid != process.tgid && p.pid.in_ns(&ns) != 0)
                .collect();
            let mut live = false;
            for group in groups {
                let threads: Vec<_> = group
                    .tasks
                    .lock_save_irq()
                    .values()
                    .filter_map(|t| t.upgrade())
                    .collect();
                if threads.iter().any(|t| {
                    !t.state
                        .load(core::sync::atomic::Ordering::Acquire)
                        .is_finished()
                        || t.sched_data.lock_save_irq().is_none()
                }) {
                    live = true;
                    group.deliver_signal(SigId::SIGKILL);
                }
            }
            if !live {
                break;
            }
            crate::drivers::timer::sleep(core::time::Duration::from_millis(1)).await;
        }
        // External setns parents retain zombie wait statuses. Do not let this
        // reaper exit until wait consumes them or parent exit reparents them.
        // Retained pidfds/PGIDs must not hold this barrier open.
        ns.wait_reaped().await;
    }

    // If this process was created with `CLONE_VFORK`, the parent may resume as
    // soon as we are guaranteed not to run in the shared address space again.
    process.complete_vfork();

    // Serialize reparenting with clone publication and other group exits.
    let _pid_op = super::pid_namespace::PID_OPS.lock_save_irq();
    let parent = process
        .parent
        .lock_save_irq()
        .as_ref()
        .and_then(|p| p.upgrade())
        .expect("live reaper");
    // Reparent to the closest living namespace reaper, never a sibling domain.
    {
        let our_children = core::mem::take(&mut *process.children.lock_save_irq());
        process.child_notifiers.reparent(process.tgid);
        for (tgid, our_child) in our_children {
            let init = reaper_for(&our_child.pid, process.tgid);
            *our_child.parent.lock_save_irq() = Some(Arc::downgrade(&init));
            init.children.lock_save_irq().insert(tgid, our_child);
        }
    }

    parent.child_notifiers.child_update(task, exit_code);

    parent.queue_signal(SigId::SIGCHLD);

    // 5. This thread is now finished.
    sched::current_work().state.finish();

    // NOTE: that the scheduler will never execute the task again since it's
    // state is set to Finished.
}

pub(super) fn reaper_for(
    pid: &super::pid_namespace::PidIdentity,
    exiting: Tgid,
) -> Arc<ThreadGroup> {
    let mut ns = Some(pid.namespace());
    while let Some(current) = ns {
        if let Some(reaper) = current.reaper()
            && reaper.tgid != exiting
            && reaper.tgid.0 != pid.global.0
        {
            return reaper;
        }
        ns = current.parent.clone();
    }
    ThreadGroup::get(Tgid::init()).expect("initial reaper")
}

pub fn kernel_exit_with_signal(ctx: &mut ProcessCtx, signal: SigId, core: bool) {
    let task = ctx.shared().clone();
    // Fatal signals replace suspended syscall work; exiting PID 1 may need to
    // sleep until all namespace descendants have left their run queues.
    drop(ctx.task_mut().ctx.take_kernel_work());
    drop(ctx.task_mut().ctx.take_signal_work());
    ctx.task_mut()
        .ctx
        .put_kernel_work(alloc::boxed::Box::pin(async move {
            do_exit_group(&task, ChildState::SignalExit { signal, core }).await;
        }));
}

pub async fn sys_exit_group(ctx: &ProcessCtx, exit_code: usize) -> Result<usize> {
    ptrace_stop(ctx, TracePoint::Exit).await;

    do_exit_group(
        ctx.shared(),
        ChildState::NormalExit {
            code: exit_code as _,
        },
    )
    .await;

    Ok(0)
}

pub async fn sys_exit(ctx: &mut ProcessCtx, exit_code: usize) -> Result<usize> {
    // Honour CLONE_CHILD_CLEARTID: clear the user TID word and futex-wake any waiters.
    let ptr = ctx.task_mut().child_tid_ptr.take();

    ptrace_stop(ctx, TracePoint::Exit).await;

    if let Some(ptr) = ptr {
        copy_to_user(ptr, 0u32).await?;

        if let Ok(key) = FutexKey::new_shared(ctx, ptr) {
            futex::wake_key(1, key, u32::MAX);
        } else {
            warn!("Failed to get futex wake key on sys_exit");
        }
    }
    let task = ctx.shared();
    let process = Arc::clone(&task.process);
    let last = {
        let mut tasks = process.tasks.lock_save_irq();
        tasks.remove(&task.tid);
        TASK_LIST.lock_save_irq().remove(&task.tid);
        !tasks.values().any(|t| t.upgrade().is_some())
    };
    if last {
        do_exit_group(
            task,
            ChildState::NormalExit {
                code: exit_code as _,
            },
        )
        .await;
    } else {
        sched::current_work().state.finish();
    }
    Ok(0)
}
