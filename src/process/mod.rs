use crate::{arch::ArchImpl, kernel::cpu_id::CpuId, sync::SpinLock};
use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
};
use core::fmt::Display;
use creds::Credentials;
use fd_table::FileDescriptorTable;
use libkernel::{VirtualMemory, fs::Inode};
use libkernel::{fs::pathbuf::PathBuf, memory::proc_vm::ProcessVM};
use thread_group::{Tgid, ThreadGroup};

pub mod caps;
pub mod clone;
pub mod creds;
pub mod ctx;
pub mod exec;
pub mod exit;
pub mod fd_table;
pub mod owned;
pub mod prctl;
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

    fn idle_for_cpu() -> Tid {
        Self(CpuId::this().value() as _)
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
            tid: Tid(CpuId::this().value() as _),
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

pub struct Task {
    pub tid: Tid,
    pub comm: Arc<SpinLock<Comm>>,
    pub process: Arc<ThreadGroup>,
    pub vm: Arc<SpinLock<ProcVM>>,
    pub cwd: Arc<SpinLock<(Arc<dyn Inode>, PathBuf)>>,
    pub root: Arc<SpinLock<(Arc<dyn Inode>, PathBuf)>>,
    pub creds: SpinLock<Credentials>,
    pub fd_table: Arc<SpinLock<FileDescriptorTable>>,
    pub state: Arc<SpinLock<TaskState>>,
    pub last_cpu: SpinLock<CpuId>,
}

impl Task {
    pub fn is_idle_task(&self) -> bool {
        self.process.tgid.is_idle()
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
