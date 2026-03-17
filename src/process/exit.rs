use super::{
    TASK_LIST, Task,
    ptrace::{TracePoint, ptrace_stop},
    thread_group::{ProcessState, Tgid, ThreadGroup, signal::SigId, wait::ChildState},
    threading::futex::{self, key::FutexKey},
};
use crate::memory::uaccess::copy_to_user;
use crate::sched::syscall_ctx::ProcessCtx;
use crate::sched::{self};
use alloc::vec::Vec;
use libkernel::error::Result;
use log::warn;
use ringbuf::Arc;

pub fn do_exit_group(task: &Arc<Task>, exit_code: ChildState) {
    let process = Arc::clone(&task.process);

    if process.tgid.is_init() {
        panic!("Attempted to kill init");
    }

    let parent = process
        .parent
        .lock_save_irq()
        .as_ref()
        .and_then(|x| x.upgrade())
        .unwrap();

    {
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

    // Signal all other threads in the group to terminate. We iterate over Weak
    // pointers and upgrade them.
    for thread_weak in process.tasks.lock_save_irq().values() {
        if let Some(other_thread) = thread_weak.upgrade() {
            // Don't signal ourselves
            if other_thread.tid != task.tid {
                // TODO: Send an IPI/Signal to halt execution now. For now, just
                // wait for the scheduler to never schedule any of it's tasks
                // again.
                other_thread.state.finish();
            }
        }
    }

    // TODO: For a UMP system, the above is sufficient, however on SMP, we need
    // to wait for all the processes to have stopped execution before tearing
    // down the address-space, etc.

    // Reparent children to `init`
    {
        let mut our_children = process.children.lock_save_irq();

        let init = ThreadGroup::get(Tgid::init()).expect("Could not find init process");

        let mut init_children = init.children.lock_save_irq();

        let mut our_children: Vec<_> = core::mem::take(&mut *our_children).into_iter().collect();

        for (tgid, our_child) in our_children.drain(..) {
            *our_child.parent.lock_save_irq() = Some(Arc::downgrade(&init));

            init_children.insert(tgid, our_child);
        }
    }

    parent.children.lock_save_irq().remove(&process.tgid);

    parent.child_notifiers.child_update(process.tgid, exit_code);

    parent
        .pending_signals
        .lock_save_irq()
        .set_signal(SigId::SIGCHLD);

    // 5. This thread is now finished.
    sched::current_work().state.finish();

    // NOTE: that the scheduler will never execute the task again since it's
    // state is set to Finished.
}

pub fn kernel_exit_with_signal(task: Arc<Task>, signal: SigId, core: bool) {
    do_exit_group(&task, ChildState::SignalExit { signal, core });
}

pub async fn sys_exit_group(ctx: &ProcessCtx, exit_code: usize) -> Result<usize> {
    ptrace_stop(ctx, TracePoint::Exit).await;

    do_exit_group(
        ctx.shared(),
        ChildState::NormalExit {
            code: exit_code as _,
        },
    );

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
    let mut tasks_lock = process.tasks.lock_save_irq();

    // How many threads are left? We must count live ones.
    let live_tasks = tasks_lock
        .values()
        .filter(|t| t.upgrade().is_some())
        .count();

    TASK_LIST.lock_save_irq().remove(&task.descriptor());

    if live_tasks <= 1 {
        // We are the last task. This is equivalent to an exit_group. The exit
        // code for an implicit exit_group is often 0.
        drop(tasks_lock);

        // NOTE: We don't need to worry about a race condition here. Since
        // we've established we're the only thread and we're executing a
        // sys_exit, there can absolutely be no way that a new thread can be
        // spawned on this process while the thread_lock is released.
        do_exit_group(
            task,
            ChildState::NormalExit {
                code: exit_code as _,
            },
        );

        Ok(0)
    } else {
        // Mark our own state as finished.
        sched::current_work().state.finish();

        // Remove ourself from the process's thread list.
        tasks_lock.remove(&task.tid);

        // 3. This thread stops executing forever. The task struct will be
        // deallocated when the last Arc<Task> is dropped (e.g., by the
        // scheduler).
        Ok(0)
    }
}
