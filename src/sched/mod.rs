use crate::arch::{Arch, ArchImpl};
use crate::drivers::timer::now;
#[cfg(feature = "smp")]
use crate::interrupts::cpu_messenger::{Message, message_cpu};
use crate::kernel::cpu_id::CpuId;
use crate::process::owned::OwnedTask;
use crate::{per_cpu_private, per_cpu_shared, process::TASK_LIST};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::fmt::Debug;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use core::task::Waker;
use core::time::Duration;
use current::{CUR_TASK_PTR, current_task};
use libkernel::error::Result;
use log::warn;
use runqueue::RunQueue;
use sched_task::{RunnableTask, Work};
use waker::create_waker;

pub mod current;
mod runqueue;
pub mod sched_task;
pub mod uspc_ret;
pub mod waker;

pub static NUM_CONTEXT_SWITCHES: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Default)]
pub struct CpuStat<T>
where
    T: Debug + Default,
{
    pub user: T,
    pub nice: T,
    pub system: T,
    pub idle: T,
    pub iowait: T,
    pub irq: T,
    pub softirq: T,
    pub steal: T,
    pub guest: T,
    pub guest_nice: T,
}

impl CpuStat<AtomicUsize> {
    pub fn to_usize(&self) -> CpuStat<usize> {
        CpuStat {
            user: self.user.load(Ordering::Relaxed),
            nice: self.nice.load(Ordering::Relaxed),
            system: self.system.load(Ordering::Relaxed),
            idle: self.idle.load(Ordering::Relaxed),
            iowait: self.iowait.load(Ordering::Relaxed),
            irq: self.irq.load(Ordering::Relaxed),
            softirq: self.softirq.load(Ordering::Relaxed),
            steal: self.steal.load(Ordering::Relaxed),
            guest: self.guest.load(Ordering::Relaxed),
            guest_nice: self.guest_nice.load(Ordering::Relaxed),
        }
    }
}

per_cpu_shared! {
    pub static CPU_STAT: CpuStat<AtomicUsize> = CpuStat::default;
}

pub fn get_cpu_stat(cpu_id: CpuId) -> CpuStat<usize> {
    CPU_STAT.get_by_cpu(cpu_id.value()).to_usize()
}

per_cpu_private! {
    static SCHED_STATE: SchedState = SchedState::new;
}

per_cpu_shared! {
    pub static SHARED_SCHED_STATE: SharedSchedState = SharedSchedState::new;
}

/// Default time-slice assigned to runnable tasks.
const DEFAULT_TIME_SLICE: Duration = Duration::from_millis(4);

/// Fixed-point configuration for virtual-time accounting.
/// We now use a 65.63 format (65 integer bits, 63 fractional bits) as
/// recommended by the EEVDF paper to minimise rounding error accumulation.
pub const VT_FIXED_SHIFT: u32 = 63;
pub const VT_ONE: u128 = 1u128 << VT_FIXED_SHIFT;
/// Tolerance used when comparing virtual-time values (see EEVDF, Fixed-Point Arithmetic).
/// Two virtual-time instants whose integer parts differ by no more than this constant are considered equal.
pub const VCLOCK_EPSILON: u128 = VT_ONE;

/// Scheduler base weight to ensure tasks always have a strictly positive
/// scheduling weight. The value is added to a task's priority to obtain its
/// effective weight (`w_i` in EEVDF paper).
pub const SCHED_WEIGHT_BASE: i32 = 1024;

/// Schedule a new task.
///
/// This function is the core of the kernel's scheduler. It is responsible for
/// deciding which process to run next.
///
/// # Logic:
/// 1. Finds the highest-priority, `Runnable` process in the system.
/// 2. The idle task (PID 0, lowest priority) serves as a fallback if no other
///    process is runnable.
/// 3. If the selected process is the same as the currently running one, no
///    switch occurs.
/// 4. If a new process is selected, it handles the state transitions (`Running`
///   > `Runnable` for the old task, `Runnable` > `Running` for the new task)
///   > and performs the architecture-specific context switch.
///
/// # Returns
///
/// Nothing, but the CPU context will be set to the next runnable task. See
/// `userspace_return` for how this is invoked.
fn schedule() {
    // Reentrancy Check
    if SCHED_STATE.try_borrow_mut().is_none() {
        warn!(
            "Scheduler reentrancy detected on CPU {}",
            CpuId::this().value()
        );
        return;
    }

    let deferred = SCHED_STATE.borrow_mut().do_schedule();

    // Drop the old RunnableTask outside the SCHED_STATE borrow. This ensures
    // that any destructors that may be called by dropping the task will be
    // called without SCHED_STATE borrowed, e.g. closeing the other end of a
    // pipe.
    drop(deferred);
}

pub fn spawn_kernel_work(fut: impl Future<Output = ()> + 'static + Send) {
    current_task().ctx.put_kernel_work(Box::pin(fut));
}

#[cfg(feature = "smp")]
fn get_best_cpu() -> CpuId {
    let r = 0..ArchImpl::cpu_count();
    r.min_by(|&x, &y| {
        // TODO: Find a way to calculate already assigned affinities and account for that
        let info_x = SHARED_SCHED_STATE.get_by_cpu(x);
        let info_y = SHARED_SCHED_STATE.get_by_cpu(y);
        let weight_x = info_x.total_runq_weight.load(Ordering::Relaxed);
        let weight_y = info_y.total_runq_weight.load(Ordering::Relaxed);
        weight_x.cmp(&weight_y)
    })
    .map(CpuId::from_value)
    .unwrap_or_else(|| {
        warn!("No CPUs found when trying to get best CPU! Defaulting to CPU 0");
        CpuId::from_value(0)
    })
}

/// Insert the given task onto a CPU's run queue.
pub fn insert_work(work: Arc<Work>) {
    SCHED_STATE.borrow_mut().run_q.add_work(work);
}

#[cfg(feature = "smp")]
pub fn insert_work_cross_cpu(work: Arc<Work>) {
    let last_cpu = work
        .sched_data
        .lock_save_irq()
        .as_ref()
        .map(|s| s.last_cpu)
        .unwrap_or(usize::MAX);
    let cpu = if last_cpu == usize::MAX {
        get_best_cpu()
    } else {
        CpuId::from_value(last_cpu)
    };
    if cpu == CpuId::this() {
        SCHED_STATE.borrow_mut().run_q.add_work(work);
    } else {
        message_cpu(cpu, Message::EnqueueWork(work)).expect("Failed to send task to CPU");
    }
}

#[cfg(not(feature = "smp"))]
pub fn insert_work_cross_cpu(task: Arc<Work>) {
    insert_work(task);
}

pub struct SchedState {
    run_q: RunQueue,
}

unsafe impl Send for SchedState {}

impl SchedState {
    pub fn new() -> Self {
        Self {
            run_q: RunQueue::new(),
        }
    }

    /// Update the global least-tasked CPU info atomically.
    #[cfg(feature = "smp")]
    fn update_global_least_tasked_cpu_info(&self) {
        let weight = self.run_q.weight();
        SHARED_SCHED_STATE
            .get()
            .total_runq_weight
            .store(weight, Ordering::Relaxed);
    }

    #[cfg(not(feature = "smp"))]
    fn update_global_least_tasked_cpu_info(&self) {
        // No-op on single-core systems.
    }

    pub fn do_schedule(&mut self) -> Vec<RunnableTask> {
        self.update_global_least_tasked_cpu_info();

        let now_inst = now().expect("System timer not initialised");

        {
            let current = self.run_q.current_mut();

            current.task.update_accounting(Some(now_inst));

            // Reset accounting baseline after updating stats to avoid double-counting
            // the same time interval on the next scheduler tick.
            current.task.reset_last_account(now_inst);
        }

        self.run_q.schedule(now_inst)
    }
}

pub struct SharedSchedState {
    pub total_runq_weight: AtomicU64,
}

impl SharedSchedState {
    pub fn new() -> Self {
        Self {
            total_runq_weight: AtomicU64::new(0),
        }
    }
}

pub fn sched_init() {
    let init_task = OwnedTask::create_init_task();

    let init_work = Work::new(Box::new(init_task));

    {
        let mut task_list = TASK_LIST.lock_save_irq();

        task_list.insert(init_work.task.descriptor(), Arc::downgrade(&init_work));
    }

    insert_work(init_work);

    schedule();
}

pub fn sched_init_secondary() {
    // Force update_global_least_tasked_cpu_info
    SCHED_STATE.borrow().update_global_least_tasked_cpu_info();

    schedule();
}

pub fn sys_sched_yield() -> Result<usize> {
    schedule();
    Ok(0)
}

pub fn current_work() -> Arc<Work> {
    SCHED_STATE.borrow().run_q.current().task.clone()
}

pub fn current_work_waker() -> Waker {
    create_waker(current_work())
}
