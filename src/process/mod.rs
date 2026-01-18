use crate::drivers::timer::Instant;
use crate::{
    arch::ArchImpl,
    kernel::cpu_id::CpuId,
    memory::{
        PAGE_ALLOC,
        fault::{FaultResolution, handle_demand_fault},
    },
    sync::SpinLock,
};
use alloc::{
    boxed::Box,
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
};
use core::fmt::Display;
use core::sync::atomic::AtomicUsize;
use creds::Credentials;
use fd_table::FileDescriptorTable;
use libkernel::{
    UserAddressSpace, VirtualMemory,
    error::{KernelError, Result},
    fs::Inode,
    memory::{
        address::{UA, VA},
        page_alloc::PageAllocation,
        proc_vm::vmarea::AccessKind,
    },
};
use libkernel::{fs::pathbuf::PathBuf, memory::proc_vm::ProcessVM};
use ptrace::PTrace;
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
pub mod ptrace;
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
        write!(f, "{state_str}")
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
    pub ptrace: SpinLock<PTrace>,
    pub utime: AtomicUsize,
    pub stime: AtomicUsize,
    pub last_account: AtomicUsize,
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

    /// Return a new descriptor that uniquely represents this task in the
    /// system.
    pub fn descriptor(&self) -> TaskDescriptor {
        TaskDescriptor::from_tgid_tid(self.process.tgid, self.tid)
    }

    /// Get a page from the task's address space, in an atomic fashion - i.e.
    /// with the process address space locked.
    ///
    /// Handle any faults such that the page will be resident in memory and return
    /// an incremented refcount for the page such that it will not be free'd until
    /// the returned allocation handle is dropped.
    ///
    /// SAFETY: The caller *must* guarantee that the returned page will only be
    /// used as described in `access_kind`. i.e. if `AccessKind::Read` is passed
    /// but data is written to this page, *bad* things will happen.
    pub async unsafe fn get_page(
        &self,
        va: UA,
        access_kind: AccessKind,
    ) -> Result<PageAllocation<'static, ArchImpl>> {
        let va = VA::from_value(va.value());

        let mut fut = None;

        loop {
            if let Some(fut) = fut.take() {
                // Handle async fault.
                Box::into_pin(fut).await?;
            }

            {
                let mut vm = self.vm.lock_save_irq();

                if let Some(pa) = vm.mm_mut().address_space_mut().translate(va) {
                    let region = pa.pfn.as_phys_range();

                    if match access_kind {
                        AccessKind::Read => pa.perms.is_read(),
                        AccessKind::Write => pa.perms.is_write(),
                        AccessKind::Execute => pa.perms.is_execute(),
                    } {
                        let alloc = unsafe { PAGE_ALLOC.get().unwrap().alloc_from_region(region) };
                        // Increase refcount on this page, ensuring it isn't reused
                        // while we copy the data.
                        let ret = alloc.clone();

                        // The original allocation is still owned by the address
                        // space.
                        alloc.leak();

                        return Ok(ret);
                    }
                }
            }

            // Try to handle the fault.
            match handle_demand_fault(self.vm.clone(), va, access_kind)? {
                // Resolved the fault.   Try again
                FaultResolution::Resolved => continue,
                FaultResolution::Denied => return Err(KernelError::Fault),
                FaultResolution::Deferred(future) => {
                    fut = Some(future);
                    continue;
                }
            }
        }
    }

    pub fn update_utime(&self, now: Instant) {
        let now = now.user_normalized();
        let now = now.ticks() as usize;
        let last_account = self
            .last_account
            .load(core::sync::atomic::Ordering::Relaxed);
        let delta = now.saturating_sub(last_account);
        self.utime
            .fetch_add(delta, core::sync::atomic::Ordering::Relaxed);
        self.process
            .utime
            .fetch_add(delta, core::sync::atomic::Ordering::Relaxed);
        self.last_account
            .store(now, core::sync::atomic::Ordering::Relaxed);
    }

    pub fn update_stime(&self, now: Instant) {
        let now = now.user_normalized();
        let now = now.ticks() as usize;
        let last_account = self
            .last_account
            .load(core::sync::atomic::Ordering::Relaxed);
        let delta = now.saturating_sub(last_account);
        self.stime
            .fetch_add(delta, core::sync::atomic::Ordering::Relaxed);
        self.process
            .stime
            .fetch_add(delta, core::sync::atomic::Ordering::Relaxed);
        self.last_account
            .store(now, core::sync::atomic::Ordering::Relaxed);
    }

    pub fn reset_last_account(&self, now: Instant) {
        let now = now.user_normalized();
        let now = now.ticks() as usize;
        self.last_account
            .store(now, core::sync::atomic::Ordering::Relaxed);
        self.last_account
            .store(now, core::sync::atomic::Ordering::Relaxed);
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
