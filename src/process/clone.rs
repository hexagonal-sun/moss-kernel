use crate::{
    process::{TASK_LIST, Task, TaskState},
    sched::{self, current_task},
    sync::SpinLock,
};
use bitflags::bitflags;
use libkernel::{
    error::{KernelError, Result},
    memory::address::UA,
};
use ringbuf::Arc;

use super::{ctx::Context, thread_group::signal::SigSet};

bitflags! {
    #[derive(Debug)]
    pub struct CloneFlags: u32 {
        const CLONE_VM = 0x100;
        const CLONE_FS = 0x200;
        const CLONE_FILES = 0x400;
        const CLONE_SIGHAND = 0x800;
        const CLONE_PTRACE = 0x2000;
        const CLONE_VFORK = 0x4000;
        const CLONE_PARENT = 0x8000;
        const CLONE_THREAD = 0x10000;
        const CLONE_NEWNS = 0x20000;
        const CLONE_SYSVSEM = 0x40000;
        const CLONE_SETTLS = 0x80000;
        const CLONE_PARENT_SETTID = 0x100000;
        const CLONE_CHILD_CLEARTID = 0x200000;
        const CLONE_DETACHED = 0x400000;
        const CLONE_UNTRACED = 0x800000;
        const CLONE_CHILD_SETTID = 0x01000000;
        const CLONE_NEWCGROUP = 0x02000000;
        const CLONE_NEWUTS = 0x04000000;
        const CLONE_NEWIPC = 0x08000000;
        const CLONE_NEWUSER = 0x10000000;
        const CLONE_NEWPID = 0x20000000;
        const CLONE_NEWNET = 0x40000000;
        const CLONE_IO = 0x80000000;
    }
}

pub async fn sys_clone(
    flags: u32,
    newsp: UA,
    _arent_tidptr: UA,
    _child_tidptr: UA,
    tls: usize,
) -> Result<usize> {
    let flags = CloneFlags::from_bits_truncate(flags);

    let new_task = {
        let current_task = current_task();

        let mut user_ctx = *current_task.ctx.lock_save_irq().user();

        // TODO: Make this arch indepdenant. The child returns '0' on clone.
        user_ctx.x[0] = 0;

        if flags.contains(CloneFlags::CLONE_SETTLS) {
            // TODO: Make this arch indepdenant.
            user_ctx.tpid_el0 = tls as _;
        }

        let (tg, tid) = if flags.contains(CloneFlags::CLONE_THREAD) {
            if !flags.contains(CloneFlags::CLONE_SIGHAND & CloneFlags::CLONE_VM) {
                // CLONE_THREAD requires both CLONE_SIGHAND and CLONE_VM to be
                // set.
                return Err(KernelError::InvalidValue);
            }
            user_ctx.sp_el0 = newsp.value() as _;

            (
                // A new task whtin this thread group.
                current_task.process.clone(),
                current_task.process.next_tid(),
            )
        } else {
            let tgid_parent = if flags.contains(CloneFlags::CLONE_PARENT) {
                // Use the parnent's parent as the new parent.
                current_task
                    .process
                    .parent
                    .lock_save_irq()
                    .clone()
                    .and_then(|p| p.upgrade())
                    // We cannot call CLONE_PARENT on the init process (which
                    // should be the only process which doesn't have a parent).
                    .ok_or(KernelError::InvalidValue)?
            } else {
                current_task.process.clone()
            };

            tgid_parent.new_child(flags.contains(CloneFlags::CLONE_SIGHAND))
        };

        let vm = if flags.contains(CloneFlags::CLONE_VM) {
            current_task.vm.clone()
        } else {
            Arc::new(SpinLock::new(
                current_task.vm.lock_save_irq().clone_as_cow()?,
            ))
        };

        let files = if flags.contains(CloneFlags::CLONE_FILES) {
            current_task.fd_table.clone()
        } else {
            Arc::new(SpinLock::new(
                current_task.fd_table.lock_save_irq().clone_for_exec(),
            ))
        };

        let cwd = if flags.contains(CloneFlags::CLONE_FS) {
            current_task.cwd.clone()
        } else {
            Arc::new(SpinLock::new(current_task.cwd.lock_save_irq().clone()))
        };

        let root = if flags.contains(CloneFlags::CLONE_FS) {
            current_task.root.clone()
        } else {
            Arc::new(SpinLock::new(current_task.root.lock_save_irq().clone()))
        };

        let creds = current_task.creds.lock_save_irq().clone();

        let new_sigmask = *current_task.sig_mask.lock_save_irq();

        Task {
            tid,
            comm: Arc::new(SpinLock::new(*current_task.comm.lock_save_irq())),
            process: tg,
            vm,
            fd_table: files,
            cwd,
            root,
            creds: SpinLock::new(creds),
            ctx: SpinLock::new(Context::from_user_ctx(user_ctx)),
            priority: current_task.priority,
            sig_mask: SpinLock::new(new_sigmask),
            pending_signals: SpinLock::new(SigSet::empty()),
            vruntime: SpinLock::new(0),
            exec_start: SpinLock::new(None),
            deadline: SpinLock::new(*current_task.deadline.lock_save_irq()),
            state: Arc::new(SpinLock::new(TaskState::Runnable)),
            last_run: SpinLock::new(None),
            robust_list: SpinLock::new(None),
        }
    };

    TASK_LIST
        .lock_save_irq()
        .insert(new_task.descriptor(), Arc::downgrade(&new_task.state));

    let tid = new_task.tid;

    sched::insert_task(Arc::new(new_task));

    Ok(tid.value() as _)
}
