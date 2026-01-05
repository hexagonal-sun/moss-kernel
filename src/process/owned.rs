use core::ops::Deref;

use super::{
    Comm, Task, TaskState, Tid,
    creds::Credentials,
    ctx::{Context, UserCtx},
    fd_table::FileDescriptorTable,
    thread_group::{
        Tgid,
        builder::ThreadGroupBuilder,
        signal::{SigId, SigSet, SignalActionState},
    },
    threading::RobustListHead,
};
use crate::{
    arch::{Arch, ArchImpl},
    fs::DummyInode,
    kernel::cpu_id::CpuId,
    sync::SpinLock,
};
use alloc::sync::Arc;
use core::sync::atomic::AtomicBool;
use libkernel::{
    VirtualMemory,
    fs::pathbuf::PathBuf,
    memory::{
        address::{TUA, VA},
        proc_vm::{ProcessVM, vmarea::VMArea},
    },
};

/// Task state which is exclusively owned by this CPU/runqueue, it is not shared
/// between other tasks and can therefore be access lock-free.
pub struct OwnedTask {
    pub ctx: Context,
    pub sig_mask: SigSet,
    pub pending_signals: SigSet,
    pub priority: Option<i8>,
    pub robust_list: Option<TUA<RobustListHead>>,
    pub child_tid_ptr: Option<TUA<u32>>,
    pub t_shared: Arc<Task>,
}

unsafe impl Send for OwnedTask {}
unsafe impl Sync for OwnedTask {}

impl Deref for OwnedTask {
    type Target = Task;

    fn deref(&self) -> &Self::Target {
        &self.t_shared
    }
}

impl OwnedTask {
    pub fn create_idle_task(
        addr_space: <ArchImpl as VirtualMemory>::ProcessAddressSpace,
        user_ctx: UserCtx,
        code_map: VMArea,
    ) -> Self {
        // SAFETY: The code page will have been mapped corresponding to the VMA.
        let vm = unsafe { ProcessVM::from_vma_and_address_space(code_map, addr_space) };

        let thread_group_builder = ThreadGroupBuilder::new(Tgid::idle())
            .with_priority(i8::MIN)
            .with_sigstate(Arc::new(SpinLock::new(SignalActionState::new_ignore())));

        let task = Task {
            tid: Tid::idle_for_cpu(),
            comm: Arc::new(SpinLock::new(Comm::new("idle"))),
            process: thread_group_builder.build(),
            state: Arc::new(SpinLock::new(TaskState::Runnable)),
            cwd: Arc::new(SpinLock::new((Arc::new(DummyInode {}), PathBuf::new()))),
            root: Arc::new(SpinLock::new((Arc::new(DummyInode {}), PathBuf::new()))),
            creds: SpinLock::new(Credentials::new_root()),
            vm: Arc::new(SpinLock::new(vm)),
            fd_table: Arc::new(SpinLock::new(FileDescriptorTable::new())),
            last_cpu: SpinLock::new(CpuId::this()),
            ptrace: AtomicBool::new(false),
        };

        Self {
            priority: Some(i8::MIN),
            ctx: Context::from_user_ctx(user_ctx),
            sig_mask: SigSet::empty(),
            pending_signals: SigSet::empty(),
            robust_list: None,
            child_tid_ptr: None,
            t_shared: Arc::new(task),
        }
    }

    pub fn create_init_task() -> Self {
        let task = Task {
            tid: Tid(1),
            comm: Arc::new(SpinLock::new(Comm::new("init"))),
            process: ThreadGroupBuilder::new(Tgid::init()).build(),
            state: Arc::new(SpinLock::new(TaskState::Runnable)),
            cwd: Arc::new(SpinLock::new((Arc::new(DummyInode {}), PathBuf::new()))),
            root: Arc::new(SpinLock::new((Arc::new(DummyInode {}), PathBuf::new()))),
            creds: SpinLock::new(Credentials::new_root()),
            vm: Arc::new(SpinLock::new(
                ProcessVM::empty().expect("Could not create init process's VM"),
            )),
            fd_table: Arc::new(SpinLock::new(FileDescriptorTable::new())),
            last_cpu: SpinLock::new(CpuId::this()),
            ptrace: AtomicBool::new(false),
        };

        Self {
            pending_signals: SigSet::empty(),
            sig_mask: SigSet::empty(),
            priority: None,
            ctx: Context::from_user_ctx(<ArchImpl as Arch>::new_user_context(
                VA::null(),
                VA::null(),
            )),
            robust_list: None,
            child_tid_ptr: None,
            t_shared: Arc::new(task),
        }
    }

    pub fn priority(&self) -> i8 {
        self.priority
            .unwrap_or_else(|| *self.process.priority.lock_save_irq())
    }

    pub fn set_priority(&mut self, priority: i8) {
        self.priority = Some(priority);
    }

    pub fn raise_task_signal(&mut self, signal: SigId) {
        self.pending_signals.insert(signal.into());
    }

    /// Take a pending signal from this task's pending signal queue, or the
    /// process's pending signal queue, while repsecting the signal mask.
    pub fn take_signal(&mut self) -> Option<SigId> {
        self.pending_signals.take_signal(self.sig_mask).or_else(|| {
            self.process
                .pending_signals
                .lock_save_irq()
                .take_signal(self.sig_mask)
        })
    }
}
