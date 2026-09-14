use super::owned::OwnedTask;
use super::ptrace::{PTrace, TracePoint, ptrace_stop};
use super::{ITimers, Tid, VmHandle};
use super::{
    ctx::Context,
    thread_group::signal::{AtomicSigSet, SigSet},
};
use crate::memory::uaccess::copy_to_user;
use crate::sched::sched_task::Work;
use crate::sched::syscall_ctx::ProcessCtx;
use crate::{
    process::{TASK_LIST, Task},
    sched::{self},
    sync::SpinLock,
};
use alloc::boxed::Box;
use bitflags::bitflags;
use core::sync::atomic::AtomicUsize;
use libkernel::memory::address::TUA;
use libkernel::{
    error::{KernelError, Result},
    memory::address::UA,
    sync::waker_set::WakerSet,
};
use ringbuf::Arc;

pub static NUM_FORKS: AtomicUsize = AtomicUsize::new(0);

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
    ctx: &ProcessCtx,
    flags: u32,
    newsp: UA,
    parent_tidptr: TUA<u32>,
    child_tidptr: TUA<u32>,
    tls: usize,
) -> Result<usize> {
    let flags = CloneFlags::from_bits_truncate(flags);
    let unsupported_namespaces = CloneFlags::CLONE_NEWCGROUP
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWIPC
        | CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWNET;
    // Unsupported namespace kinds must not silently share the global domain.
    if flags.intersects(unsupported_namespaces)
        || flags.contains(CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_FS)
        || (flags.contains(CloneFlags::CLONE_NEWUSER)
            && flags.intersects(CloneFlags::CLONE_FS | CloneFlags::CLONE_THREAD))
        || (flags.contains(CloneFlags::CLONE_SIGHAND) && !flags.contains(CloneFlags::CLONE_VM))
        || (flags.contains(CloneFlags::CLONE_THREAD)
            && !flags.contains(CloneFlags::CLONE_SIGHAND | CloneFlags::CLONE_VM))
    {
        return Err(KernelError::InvalidValue);
    }

    let mut creds = ctx.shared().creds.lock_save_irq().clone();
    if flags.contains(CloneFlags::CLONE_NEWUSER) {
        super::namespace::check_userns_create(ctx.shared())?;
        let ns = super::user_namespace::UserNamespace::create(&creds)?;
        creds.enter_user_ns(ns);
    }
    if flags.contains(CloneFlags::CLONE_NEWNS) {
        creds
            .caps()
            .check_capable(libkernel::proc::caps::CapabilitiesFlags::CAP_SYS_ADMIN)?;
    }

    let trace_point = if flags.contains(CloneFlags::CLONE_THREAD) {
        TracePoint::Clone
    } else {
        TracePoint::Fork
    };

    // TODO: differentiate between `TracePoint::Fork`, `TracePoint::Clone` and
    // `TracePoint::VFork`.
    let should_trace_new_tsk = ptrace_stop(ctx, trace_point).await;

    let mut new_task = {
        let tid = Tid::next_tid();

        let current_task = ctx.task();

        let mut user_ctx = *current_task.ctx.user();

        // TODO: Make this arch independent. The child returns '0' on clone.
        user_ctx.x[0] = 0;
        if !newsp.is_null() {
            user_ctx.sp_el0 = newsp.value() as _;
        }

        if flags.contains(CloneFlags::CLONE_SETTLS) {
            // TODO: Make this arch independent.
            user_ctx.tpid_el0 = tls as _;
        }

        let tg = if flags.contains(CloneFlags::CLONE_THREAD) {
            if !flags.contains(CloneFlags::CLONE_SIGHAND | CloneFlags::CLONE_VM) {
                // CLONE_THREAD requires both CLONE_SIGHAND and CLONE_VM to be
                // set.
                return Err(KernelError::InvalidValue);
            }

            // A new task within this thread group.
            current_task.process.clone()
        } else {
            let tgid_parent = if flags.contains(CloneFlags::CLONE_PARENT) {
                // Use the parent's parent as the new parent.
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

            let child = tgid_parent.new_child(flags.contains(CloneFlags::CLONE_SIGHAND), tid);
            // CLONE_PARENT changes the parent relationship, not which program
            // or session is inherited from the cloning task.
            *child.executable.lock_save_irq() =
                current_task.process.executable.lock_save_irq().clone();
            *child.sid.lock_save_irq() = *current_task.process.sid.lock_save_irq();
            child
        };

        let vm = if flags.contains(CloneFlags::CLONE_VM) {
            if flags.contains(CloneFlags::CLONE_THREAD) {
                current_task.vm.clone()
            } else {
                Arc::new(VmHandle::from_shared(current_task.vm.shared_vm()))
            }
        } else {
            let proc_vm = current_task.vm.shared_vm();
            Arc::new(VmHandle::new(proc_vm.lock_save_irq().clone_as_cow()?))
        };

        let files = if flags.contains(CloneFlags::CLONE_FILES) {
            current_task.fd_table.clone()
        } else {
            Arc::new(SpinLock::new(current_task.fd_table.lock_save_irq().clone()))
        };

        let fs = if flags.contains(CloneFlags::CLONE_FS) {
            current_task.fs()
        } else {
            current_task.fs().duplicate()
        };
        let mut mount_ns = current_task.mount_ns();
        if flags.contains(CloneFlags::CLONE_NEWNS) {
            let (new_ns, map) = mount_ns.duplicate(creds.user_ns());
            fs.remap_mounts(&map);
            mount_ns = new_ns;
        }

        let ptrace = if flags.contains(CloneFlags::CLONE_PTRACE) || should_trace_new_tsk {
            current_task.ptrace.lock_save_irq().clone()
        } else {
            PTrace::new()
        };

        let new_sigmask = AtomicSigSet::new(current_task.sig_mask.load());

        let initial_signals = if should_trace_new_tsk {
            // When we want to trace a new task through one of
            // PTRACE_O_TRACE{FORK,VFORK,CLONE}, stop the child as soon as
            // it is created.
            AtomicSigSet::new(SigSet::SIGSTOP)
        } else {
            AtomicSigSet::empty()
        };

        OwnedTask {
            ctx: Context::from_user_ctx(user_ctx),
            priority: current_task.priority,
            robust_list: None,
            child_tid_ptr: if flags.contains(CloneFlags::CLONE_CHILD_CLEARTID)
                && !child_tidptr.is_null()
            {
                Some(child_tidptr)
            } else {
                None
            },
            t_shared: Arc::new(Task {
                tid,
                comm: Arc::new(SpinLock::new(*current_task.comm.lock_save_irq())),
                process: tg,
                vm,
                fd_table: files,
                fs: SpinLock::new(fs),
                mount_ns: SpinLock::new(mount_ns),
                i_timers: SpinLock::new(ITimers::default()),
                creds: SpinLock::new(creds),
                ptrace: SpinLock::new(ptrace),
                sig_mask: new_sigmask,
                pending_signals: initial_signals,
                signal_notifier: SpinLock::new(WakerSet::new()),
                utime: AtomicUsize::new(0),
                stime: AtomicUsize::new(0),
                last_account: AtomicUsize::new(0),
            }),
            in_syscall: false,
        }
    };

    // Linux writes the child TID in the child's address space, on its first
    // return to userspace. Doing this in the parent corrupts its COW copy.
    if flags.contains(CloneFlags::CLONE_CHILD_SETTID) {
        let tid = new_task.tid.value();
        new_task.ctx.put_kernel_work(Box::pin(async move {
            let _ = copy_to_user(child_tidptr, tid).await;
        }));
    }

    if flags.contains(CloneFlags::CLONE_VFORK) {
        new_task.process.start_vfork();
    }

    let desc = new_task.descriptor();
    let work = Work::new(Box::new(new_task));
    let vfork_process = flags
        .contains(CloneFlags::CLONE_VFORK)
        .then(|| work.process.clone());

    // Publish the parent's TID store before the child can run and clear it.
    // Like Linux put_user here, a bad TID pointer does not cancel the clone.
    if flags.contains(CloneFlags::CLONE_PARENT_SETTID) {
        let _ = copy_to_user(parent_tidptr, desc.tid.value()).await;
    }

    TASK_LIST
        .lock_save_irq()
        .insert(desc.tid(), Arc::downgrade(&work));

    work.process
        .tasks
        .lock_save_irq()
        .insert(desc.tid, Arc::downgrade(&work));

    sched::insert_work_cross_cpu(work);

    NUM_FORKS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    if let Some(vfork_process) = vfork_process {
        vfork_process.wait_for_vfork_release().await;
    }

    Ok(desc.tid.value() as _)
}
