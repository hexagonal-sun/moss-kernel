use crate::drivers::timer::Instant;
use crate::process::threading::RobustListHead;
use crate::sched::CpuId;
use crate::{
    arch::{Arch, ArchImpl},
    fs::DummyInode,
    sync::SpinLock,
};
use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
};
use core::fmt::Display;
use creds::Credentials;
use ctx::{Context, UserCtx};
use fd_table::FileDescriptorTable;
use libkernel::memory::address::TUA;
use libkernel::{VirtualMemory, fs::Inode};
use libkernel::{
    fs::pathbuf::PathBuf,
    memory::{
        address::VA,
        proc_vm::{ProcessVM, vmarea::VMArea},
    },
};
use thread_group::{
    Tgid, ThreadGroup,
    builder::ThreadGroupBuilder,
    signal::{SigId, SigSet, SignalState},
};

pub mod caps;
pub mod clone;
pub mod creds;
pub mod ctx;
pub mod exec;
pub mod exit;
pub mod fd_table;
pub mod sleep;
pub mod thread_group;
pub mod threading;

// Thread Id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tid(pub u32);

impl Tid {
    pub fn value(self) -> u32 {
        self.0
    }

    pub fn from_tgid(tgid: Tgid) -> Self {
        Self(tgid.0)
    }
}

/// A unqiue identifier for any task in the current system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskDescriptor {
    tid: Tid,
    tgid: Tgid,
}

impl TaskDescriptor {
    pub fn from_tgid_tid(tgid: Tgid, tid: Tid) -> Self {
        Self { tid, tgid }
    }

    /// Returns a descriptor for the idle task.
    pub fn this_cpus_idle() -> Self {
        Self {
            tgid: Tgid(0),
            tid: Tid(0),
        }
    }

    /// Returns a representation of a descriptor encoded in a single pointer
    /// value.
    #[cfg(target_pointer_width = "64")]
    pub fn to_ptr(self) -> *const () {
        let mut value: u64 = self.tgid.value() as _;

        value |= (self.tid.value() as u64) << 32;

        value as _
    }

    /// Returns a descriptor decoded from a single pointer value. This is the
    /// inverse of `to_ptr`.
    #[cfg(target_pointer_width = "64")]
    pub fn from_ptr(ptr: *const ()) -> Self {
        let value = ptr as u64;

        let tgid = value & 0xffffffff;
        let tid = value >> 32;

        Self {
            tgid: Tgid(tgid as _),
            tid: Tid(tid as _),
        }
    }

    pub fn is_idle(&self) -> bool {
        self.tgid.is_idle()
    }

    /// Returns the task-group ID (i.e. the PID) associated with this descriptor.
    pub fn tgid(&self) -> Tgid {
        self.tgid
    }

    /// Returns the thread ID associated with this descriptor.
    pub fn tid(&self) -> Tid {
        self.tid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Runnable,
    Woken,
    Stopped,
    Sleeping,
    Finished,
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
        };
        write!(f, "{}", state_str)
    }
}

impl TaskState {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Finished)
    }
}
pub type ProcVM = ProcessVM<<ArchImpl as VirtualMemory>::ProcessAddressSpace>;

#[derive(Copy, Clone)]
pub struct Comm([u8; 16]);

impl Comm {
    /// Create a new command name from the given string.
    /// Truncates to 15 characters if necessary, and null-terminates.
    pub fn new(name: &str) -> Self {
        let mut comm = [0u8; 16];
        let bytes = name.as_bytes();
        let len = core::cmp::min(bytes.len(), 15);
        comm[..len].copy_from_slice(&bytes[..len]);
        Self(comm)
    }

    pub fn as_str(&self) -> &str {
        let len = self.0.iter().position(|&c| c == 0).unwrap_or(16);
        core::str::from_utf8(&self.0[..len]).unwrap_or("")
    }
}

/// Scheduler base weight to ensure tasks always have a strictly positive
/// scheduling weight. The value is added to a task's priority to obtain its
/// effective weight (`w_i` in EEVDF paper).
pub const SCHED_WEIGHT_BASE: i32 = 1024;

pub struct Task {
    pub tid: Tid,
    pub comm: Arc<SpinLock<Comm>>,
    pub process: Arc<ThreadGroup>,
    pub vm: Arc<SpinLock<ProcVM>>,
    pub cwd: Arc<SpinLock<(Arc<dyn Inode>, PathBuf)>>,
    pub root: Arc<SpinLock<(Arc<dyn Inode>, PathBuf)>>,
    pub creds: SpinLock<Credentials>,
    pub fd_table: Arc<SpinLock<FileDescriptorTable>>,
    pub ctx: SpinLock<Context>,
    pub sig_mask: SpinLock<SigSet>,
    pub pending_signals: SpinLock<SigSet>,
    pub v_runtime: SpinLock<u128>,
    /// Virtual time at which the task becomes eligible (v_ei).
    pub v_eligible: SpinLock<u128>,
    /// Virtual deadline (v_di) used by the EEVDF scheduler.
    pub v_deadline: SpinLock<u128>,
    pub exec_start: SpinLock<Option<Instant>>,
    pub deadline: SpinLock<Option<Instant>>,
    pub priority: i8,
    pub last_run: SpinLock<Option<Instant>>,
    pub state: Arc<SpinLock<TaskState>>,
    pub robust_list: SpinLock<Option<TUA<RobustListHead>>>,
    pub child_tid_ptr: SpinLock<Option<TUA<u32>>>,
    pub last_cpu: SpinLock<CpuId>,
}

impl Task {
    pub fn create_idle_task(
        addr_space: <ArchImpl as VirtualMemory>::ProcessAddressSpace,
        user_ctx: UserCtx,
        code_map: VMArea,
    ) -> Self {
        // SAFETY: The code page will have been mapped corresponding to the VMA.
        let vm = unsafe { ProcessVM::from_vma_and_address_space(code_map, addr_space) };

        let thread_group_builder = ThreadGroupBuilder::new(Tgid::idle())
            .with_sigstate(Arc::new(SpinLock::new(SignalState::new_ignore())));

        Self {
            tid: Tid(0),
            comm: Arc::new(SpinLock::new(Comm::new("idle"))),
            process: thread_group_builder.build(),
            state: Arc::new(SpinLock::new(TaskState::Runnable)),
            priority: i8::MIN,
            cwd: Arc::new(SpinLock::new((Arc::new(DummyInode {}), PathBuf::new()))),
            root: Arc::new(SpinLock::new((Arc::new(DummyInode {}), PathBuf::new()))),
            creds: SpinLock::new(Credentials::new_root()),
            ctx: SpinLock::new(Context::from_user_ctx(user_ctx)),
            vm: Arc::new(SpinLock::new(vm)),
            sig_mask: SpinLock::new(SigSet::empty()),
            pending_signals: SpinLock::new(SigSet::empty()),
            v_runtime: SpinLock::new(0),
            v_eligible: SpinLock::new(0),
            v_deadline: SpinLock::new(0),
            exec_start: SpinLock::new(None),
            deadline: SpinLock::new(None),
            fd_table: Arc::new(SpinLock::new(FileDescriptorTable::new())),
            last_run: SpinLock::new(None),
            robust_list: SpinLock::new(None),
            child_tid_ptr: SpinLock::new(None),
            last_cpu: SpinLock::new(CpuId::this()),
        }
    }

    pub fn create_init_task() -> Self {
        Self {
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
            pending_signals: SpinLock::new(SigSet::empty()),
            v_runtime: SpinLock::new(0),
            v_eligible: SpinLock::new(0),
            v_deadline: SpinLock::new(0),
            exec_start: SpinLock::new(None),
            deadline: SpinLock::new(None),
            sig_mask: SpinLock::new(SigSet::empty()),
            priority: 0,
            ctx: SpinLock::new(Context::from_user_ctx(
                <ArchImpl as Arch>::new_user_context(VA::null(), VA::null()),
            )),
            last_run: SpinLock::new(None),
            robust_list: SpinLock::new(None),
            child_tid_ptr: SpinLock::new(None),
            last_cpu: SpinLock::new(CpuId::this()),
        }
    }

    pub fn is_idle_task(&self) -> bool {
        self.process.tgid.is_idle()
    }

    pub fn priority(&self) -> i8 {
        self.priority
    }

    /// Compute this task's scheduling weight.
    ///
    /// weight = priority + SCHED_WEIGHT_BASE
    /// The sum is clamped to a minimum of 1
    pub fn weight(&self) -> u32 {
        let w = self.priority as i32 + SCHED_WEIGHT_BASE;
        if w <= 0 { 1 } else { w as u32 }
    }

    pub fn set_priority(&mut self, priority: i8) {
        self.priority = priority;
    }

    pub fn pgid(&self) -> Tgid {
        self.process.tgid
    }

    pub fn tid(&self) -> Tid {
        self.tid
    }

    /// Return a new desctiptor that uniquely represents this task in the
    /// system.
    pub fn descriptor(&self) -> TaskDescriptor {
        TaskDescriptor::from_tgid_tid(self.process.tgid, self.tid)
    }

    pub fn raise_task_signal(&self, signal: SigId) {
        self.pending_signals.lock_save_irq().insert(signal.into());
    }
}

pub fn find_task_by_descriptor(descriptor: &TaskDescriptor) -> Option<Arc<Task>> {
    TASK_LIST
        .lock_save_irq()
        .get(descriptor)
        .and_then(|x| x.upgrade())
}

pub static TASK_LIST: SpinLock<BTreeMap<TaskDescriptor, Weak<Task>>> =
    SpinLock::new(BTreeMap::new());

unsafe impl Send for Task {}
unsafe impl Sync for Task {}
