use crate::arch::ArchImpl;
use crate::drivers::timer::{Instant, now};
#[cfg(feature = "smp")]
use crate::interrupts::cpu_messenger::{Message, message_cpu};
use crate::kernel::cpu_id::CpuId;
use crate::process::owned::OwnedTask;
use crate::{
    arch::Arch,
    per_cpu_private, per_cpu_shared,
    process::{TASK_LIST, TaskDescriptor, TaskState},
};
use alloc::{boxed::Box, collections::btree_map::BTreeMap, sync::Arc};
use core::fmt::Debug;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use core::time::Duration;
use current::{CUR_TASK_PTR, current_task};
use libkernel::{UserAddressSpace, error::Result};
use log::warn;
use runqueue::{RunQueue, SwitchResult};
use sched_task::SchedulableTask;

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

    SCHED_STATE.borrow_mut().do_schedule();
}

pub fn spawn_kernel_work(fut: impl Future<Output = ()> + 'static + Send) {
    current_task().ctx.put_kernel_work(Box::pin(fut));
}

/// Global atomic storing info about the least-tasked CPU.
/// First 16 bits: CPU ID
/// Next 24 bits: Weight
/// Next 24 bits: Number of waiting tasks
#[cfg(feature = "smp")]
static LEAST_TASKED_CPU_INFO: AtomicU64 = AtomicU64::new(0);
const WEIGHT_SHIFT: u32 = 16;
const WAITING_SHIFT: u32 = WEIGHT_SHIFT + 24;

#[cfg(feature = "smp")]
fn get_best_cpu() -> CpuId {
    // Get the CPU with the least number of tasks.
    let least_tasked_cpu_info = LEAST_TASKED_CPU_INFO.load(Ordering::Acquire);
    CpuId::from_value((least_tasked_cpu_info & 0xffff) as usize)
}

/// Insert the given task onto a CPU's run queue.
pub fn insert_task(task: Box<OwnedTask>) {
    SCHED_STATE
        .borrow_mut()
        .insert_into_runq(SchedulableTask::new(task));
}

#[cfg(feature = "smp")]
pub fn insert_task_cross_cpu(task: Box<OwnedTask>) {
    let cpu = get_best_cpu();
    if cpu == CpuId::this() {
        insert_task(task);
    } else {
        message_cpu(cpu, Message::PutTask(task)).expect("Failed to send task to CPU");
    }
}

#[cfg(not(feature = "smp"))]
pub fn insert_task_cross_cpu(task: Box<OwnedTask>) {
    insert_task(task);
}

pub struct SchedState {
    run_q: RunQueue,
    wait_q: BTreeMap<TaskDescriptor, Box<SchedulableTask>>,
    /// Per-CPU virtual clock (fixed-point 65.63 stored in a u128).
    /// Expressed in virtual-time units as defined by the EEVDF paper.
    vclock: u128,
    /// Real-time moment when `vclock` was last updated.
    last_update: Option<Instant>,
    /// Force a reschedule.
    force_resched: bool,
}

unsafe impl Send for SchedState {}

impl SchedState {
    pub const fn new() -> Self {
        Self {
            run_q: RunQueue::new(),
            wait_q: BTreeMap::new(),
            vclock: 0,
            last_update: None,
            force_resched: false,
        }
    }

    /// Update the global least-tasked CPU info atomically.
    #[cfg(feature = "smp")]
    fn update_global_least_tasked_cpu_info(&self) {
        fn none<T>() -> Option<T> {
            None
        }

        per_cpu_private! {
            static LAST_UPDATE: Option<Instant> = none;
        }

        // Try and throttle contention on the atomic variable.
        const MIN_COOLDOWN: Duration = Duration::from_millis(16);
        if let Some(last) = LAST_UPDATE.borrow().as_ref()
            && let Some(now) = now()
            && now - *last < MIN_COOLDOWN
        {
            return;
        }
        *LAST_UPDATE.borrow_mut() = now();

        let weight = self.run_q.weight();
        let waiting_tasks = self.wait_q.len() as u64;
        let cpu_id = CpuId::this().value() as u64;
        let new_info = (cpu_id & 0xffff)
            | ((weight & 0xffffff) << WEIGHT_SHIFT)
            | ((waiting_tasks & 0xffffff) << WAITING_SHIFT);
        let mut old_info = LEAST_TASKED_CPU_INFO.load(Ordering::Acquire);
        // Ensure we don't spin forever (possible with a larger number of CPUs)
        const MAX_RETRIES: usize = 8;
        // Ensure consistency
        for _ in 0..MAX_RETRIES {
            let old_cpu_id = old_info & 0xffff;
            let old_weight = (old_info >> WEIGHT_SHIFT) & 0xffffff;
            let old_waiting = (old_info >> WAITING_SHIFT) & 0xffffff;
            let metric = weight + (waiting_tasks * SCHED_WEIGHT_BASE as u64);
            let old_metric = old_weight + (old_waiting * SCHED_WEIGHT_BASE as u64);
            if (cpu_id == old_cpu_id && old_info != new_info) || (metric < old_metric) {
                match LEAST_TASKED_CPU_INFO.compare_exchange(
                    old_info,
                    new_info,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(x) => old_info = x,
                }
            }
        }
    }

    #[cfg(not(feature = "smp"))]
    fn update_global_least_tasked_cpu_info(&self) {
        // No-op on single-core systems.
    }

    /// Advance the per-CPU virtual clock (`vclock`) by converting the elapsed
    /// real time since the last update into 65.63-format fixed-point
    /// virtual-time units:
    ///     v += (delta t << VT_FIXED_SHIFT) /  sum w
    /// The caller must pass the current real time (`now_inst`).
    fn advance_vclock(&mut self, now_inst: Instant) {
        if let Some(prev) = self.last_update {
            let delta_real = now_inst - prev;
            if self.run_q.weight() > 0 {
                let delta_vt =
                    ((delta_real.as_nanos()) << VT_FIXED_SHIFT) / self.run_q.weight() as u128;
                self.vclock = self.vclock.saturating_add(delta_vt);
            }
        }
        self.last_update = Some(now_inst);
    }

    fn insert_into_runq(&mut self, mut new_task: Box<SchedulableTask>) {
        let now = now().expect("systimer not running");

        self.advance_vclock(now);

        new_task.inserting_into_runqueue(self.vclock);

        if let Some(current) = self.run_q.current() {
            // We force a reschedule if:
            //
            // We are currently idling, OR The new task has an earlier deadline
            // than the current task.
            if current.is_idle_task() || new_task.v_deadline < current.v_deadline {
                self.force_resched = true;
            }
        }

        self.run_q.enqueue_task(new_task);

        self.update_global_least_tasked_cpu_info();
    }

    pub fn wakeup(&mut self, desc: TaskDescriptor) {
        if let Some(task) = self.wait_q.remove(&desc) {
            self.insert_into_runq(task);
        } else {
            warn!(
                "Spurious wakeup for task {:?} on CPU {:?}",
                desc,
                CpuId::this().value()
            );
        }
    }

    pub fn do_schedule(&mut self) {
        self.update_global_least_tasked_cpu_info();
        // Update Clocks
        let now_inst = now().expect("System timer not initialised");

        self.advance_vclock(now_inst);

        let mut needs_resched = self.force_resched;

        if let Some(current) = self.run_q.current_mut() {
            current.update_accounting(Some(now_inst));
            // Reset accounting baseline after updating stats to avoid double-counting
            // the same time interval on the next scheduler tick.
            current.reset_last_account(now_inst);
            // If the current task is IDLE, we always want to proceed to the
            // scheduler core to see if a real task has arrived.
            if current.is_idle_task() {
                needs_resched = true;
            } else if current.tick(now_inst) {
                // Otherwise, check if the real task expired
                needs_resched = true;
            }
        } else {
            needs_resched = true;
        }

        if !needs_resched
            && let Some(current) = self.run_q.current()
            && matches!(*current.state.lock_save_irq(), TaskState::Running)
        {
            // Fast Path: Only return if we have a valid task (Running state),
            // it has budget, AND it's not the idle task.
            return;
        }

        // Reset the force flag for next time.
        self.force_resched = false;

        // Select Next Task.
        let next_task_desc = self.run_q.find_next_runnable_desc(self.vclock);

        match self.run_q.switch_tasks(next_task_desc, now_inst) {
            SwitchResult::AlreadyRunning => {
                // Nothing to do.
                return;
            }
            SwitchResult::Blocked { old_task } => {
                // If the blocked task has finished, allow it to drop here so it's
                // resources are released.
                if !old_task.state.lock_save_irq().is_finished() {
                    self.wait_q.insert(old_task.descriptor(), old_task);
                }
            }
            // fall-thru.
            SwitchResult::Preempted => {}
        }

        // Update all context since the task has switched.
        if let Some(new_current) = self.run_q.current_mut() {
            NUM_CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);
            ArchImpl::context_switch(new_current.t_shared.clone());
            let now = now().unwrap();
            new_current.reset_last_account(now);
            CUR_TASK_PTR.borrow_mut().set_current(&mut new_current.task);
        }
    }
}

pub fn sched_init() {
    let idle_task = ArchImpl::create_idle_task();
    let init_task = OwnedTask::create_init_task();

    init_task
        .vm
        .lock_save_irq()
        .mm_mut()
        .address_space_mut()
        .activate();

    *init_task.state.lock_save_irq() = TaskState::Runnable;

    {
        let mut task_list = TASK_LIST.lock_save_irq();

        task_list.insert(idle_task.descriptor(), Arc::downgrade(&idle_task.t_shared));
        task_list.insert(init_task.descriptor(), Arc::downgrade(&init_task.t_shared));
    }

    insert_task(Box::new(idle_task));
    insert_task(Box::new(init_task));

    schedule();
}

pub fn sched_init_secondary() {
    let idle_task = ArchImpl::create_idle_task();

    insert_task(Box::new(idle_task));
    // Force update_global_least_tasked_cpu_info
    SCHED_STATE.borrow().update_global_least_tasked_cpu_info();
}

pub fn sys_sched_yield() -> Result<usize> {
    schedule();
    Ok(0)
}
