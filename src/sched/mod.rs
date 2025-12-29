use crate::drivers::timer::{Instant, now, schedule_force_preempt, schedule_preempt};
use crate::interrupts::cpu_messenger::{Message, message_cpu};
use crate::{
    arch::{Arch, ArchImpl},
    per_cpu,
    process::{TASK_LIST, Task, TaskDescriptor, TaskState},
    sync::OnceLock,
};
use alloc::{boxed::Box, collections::btree_map::BTreeMap, sync::Arc};
use core::cmp::Ordering;
use core::sync::atomic::AtomicUsize;
use core::time::Duration;
use libkernel::{CpuOps, UserAddressSpace, error::Result};

pub mod uspc_ret;
pub mod waker;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CpuId(usize);

impl CpuId {
    pub fn this() -> CpuId {
        CpuId(ArchImpl::id())
    }

    pub fn value(&self) -> usize {
        self.0
    }
}

// TODO: arbitrary cap.
per_cpu! {
    static SCHED_STATE: SchedState = SchedState::new;
}

/// Default time-slice (in milliseconds) assigned to runnable tasks.
const DEFAULT_TIME_SLICE_MS: u64 = 4;

/// Fixed-point configuration for virtual-time accounting.
/// We now use a 65.63 format (65 integer bits, 63 fractional bits) as
/// recommended by the EEVDF paper to minimise rounding error accumulation.
pub const VT_FIXED_SHIFT: u32 = 63;
pub const VT_ONE: u128 = 1u128 << VT_FIXED_SHIFT;
/// Tolerance used when comparing virtual-time values (see EEVDF, Fixed-Point Arithmetic).
/// Two virtual-time instants whose integer parts differ by no more than this constant are considered equal.
pub const VCLOCK_EPSILON: u128 = VT_ONE;

pub fn find_task_by_descriptor(descriptor: &TaskDescriptor) -> Option<Arc<Task>> {
    if let Some(task) = SCHED_STATE.borrow().run_queue.get(descriptor) {
        return Some(task.clone());
    }
    // TODO: Ping other CPUs to find the task.
    None
}

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
    if SCHED_STATE.try_borrow_mut().is_none() {
        log::warn!(
            "Scheduler reentrancy detected on CPU {}",
            CpuId::this().value()
        );
        return;
    }
    // Mark the current task as runnable so it's considered for scheduling in
    // the next time-slice.
    {
        let task = current_task();
        let mut task_state = task.state.lock_save_irq();

        if *task_state == TaskState::Running {
            *task_state = TaskState::Runnable;
        }
    }

    let previous_task = current_task();
    let mut sched_state = SCHED_STATE.borrow_mut();

    // Bring the virtual clock up-to-date so that eligibility tests use the
    // most recent value.
    let now_inst = now().expect("System timer not initialised");
    sched_state.advance_vclock(now_inst);

    let next_task = sched_state.find_next_runnable_task();
    // if previous_task.tid != next_task.tid {
    //     let runnable_tasks = sched_state
    //         .run_queue
    //         .values()
    //         .filter(|t| *t.state.lock_save_irq() == TaskState::Runnable)
    //         .count();
    //     if matches!(*previous_task.state.lock_save_irq(), TaskState::Sleeping | TaskState::Finished) {
    //         log::debug!(
    //             "CPU {} scheduling switch due to removal from run queue: {} -> {} (runnable tasks: {runnable_tasks})",
    //             CpuId::this().value(),
    //             previous_task.tid.value(),
    //             next_task.tid.value(),
    //         );
    //     } else {
    //         log::debug!(
    //             "CPU {} scheduling switch: {} -> {} (runnable tasks: {runnable_tasks})",
    //             CpuId::this().value(),
    //             previous_task.tid.value(),
    //             next_task.tid.value()
    //         );
    //     }
    // }

    sched_state
        .switch_to_task(Some(previous_task), next_task.clone())
        .expect("Could not schedule next task");
}

pub fn spawn_kernel_work(fut: impl Future<Output = ()> + 'static + Send) {
    current_task()
        .ctx
        .lock_save_irq()
        .put_kernel_work(Box::pin(fut));
}

fn get_next_cpu() -> CpuId {
    static NEXT_CPU: AtomicUsize = AtomicUsize::new(0);

    let cpu_count = ArchImpl::cpu_count();
    let cpu_id = NEXT_CPU.fetch_add(1, core::sync::atomic::Ordering::Relaxed) % cpu_count;

    CpuId(cpu_id)
}

/// Insert the given task onto a CPU's run queue.
pub fn insert_task(task: Arc<Task>) {
    SCHED_STATE.borrow_mut().add_task(task);
}

pub fn insert_task_cross_cpu(task: Arc<Task>) {
    let cpu = get_next_cpu();
    if cpu == CpuId::this() {
        insert_task(task);
    } else {
        message_cpu(cpu.value(), Message::PutTask(task)).expect("Failed to send task to CPU");
    }
}

pub struct SchedState {
    /// Task that is currently running on this CPU (if any).
    running_task: Option<Arc<Task>>,
    // TODO: To be changed to virtual-deadline key for better performance
    // TODO: Use a red-black tree for better performance.
    pub run_queue: BTreeMap<TaskDescriptor, Arc<Task>>,
    /// Per-CPU virtual clock (fixed-point 65.63 stored in a u128).
    /// Expressed in virtual-time units as defined by the EEVDF paper.
    vclock: u128,
    /// Cached sum of weights of all tasks in the run queue (`sum w_i`).
    total_weight: u64,
    /// Real-time moment when `vclock` was last updated.
    last_update: Option<Instant>,
}

unsafe impl Send for SchedState {}

impl SchedState {
    /// Inserts `task` into this CPU's run-queue and updates all EEVDF
    /// accounting information (eligible time, virtual deadline and the cached
    /// weight sum).
    pub fn add_task(&mut self, task: Arc<Task>) {
        // Always advance the virtual clock first so that eligibility and
        // deadline calculations for the incoming task are based on the most
        // recent time stamp.
        let now_inst = now().expect("System timer not initialised");
        self.advance_vclock(now_inst);

        let desc = task.descriptor();

        if self.run_queue.contains_key(&desc) {
            return;
        }

        // A freshly enqueued task becomes eligible immediately.
        *task.v_eligible.lock_save_irq() = self.vclock;

        // Grant it an initial virtual deadline proportional to its weight.
        let q_ns: u128 = (DEFAULT_TIME_SLICE_MS as u128) * 1_000_000;
        let v_delta = (q_ns << VT_FIXED_SHIFT) / task.weight() as u128;
        let new_v_deadline = self.vclock + v_delta;
        *task.v_deadline.lock_save_irq() = new_v_deadline;

        // Since the task is not executing yet, its exec_start must be `None`.
        *task.exec_start.lock_save_irq() = None;

        if !task.is_idle_task() {
            self.total_weight = self.total_weight.saturating_add(task.weight() as u64);
        }

        // Decide whether the currently-running task must be preempted
        // immediately.
        let newcomer_eligible = {
            let v_e = *task.v_eligible.lock_save_irq();
            v_e.saturating_sub(self.vclock) <= VCLOCK_EPSILON
        };
        let preempt_now = if newcomer_eligible {
            if let Some(ref current) = self.running_task {
                let current_deadline = *current.v_deadline.lock_save_irq();
                new_v_deadline < current_deadline
            } else {
                true
            }
        } else {
            false
        };

        self.run_queue.insert(desc, task);

        // Arm an immediate preemption timer so that the interrupt
        // handler will force the actual context switch as soon as possible.
        if preempt_now {
            schedule_preempt(now_inst + Duration::from_nanos(1));
        }
    }

    /// Removes a task given its descriptor and subtracts its weight from the
    /// cached `total_weight`.  Missing descriptors are ignored.
    pub fn remove_task_with_weight(&mut self, desc: &TaskDescriptor) {
        if let Some(task) = self.run_queue.remove(desc) {
            if task.is_idle_task() {
                panic!("Cannot remove the idle task");
            }
            self.total_weight = self.total_weight.saturating_sub(task.weight() as u64);
        }
    }
    pub const fn new() -> Self {
        Self {
            running_task: None,
            run_queue: BTreeMap::new(),
            vclock: 0,
            total_weight: 0,
            last_update: None,
        }
    }

    /// Advance the per-CPU virtual clock (`vclock`) by converting the elapsed
    /// real time since the last update into 65.63-format fixed-point
    /// virtual-time units:
    ///     v += (delta t << VT_FIXED_SHIFT) /  sum w
    /// The caller must pass the current real time (`now_inst`).
    fn advance_vclock(&mut self, now_inst: Instant) {
        if let Some(prev) = self.last_update {
            let delta_real = now_inst - prev;
            if self.total_weight > 0 {
                let delta_vt =
                    ((delta_real.as_nanos()) << VT_FIXED_SHIFT) / self.total_weight as u128;
                self.vclock = self.vclock.saturating_add(delta_vt);
            }
        }
        self.last_update = Some(now_inst);
    }

    fn switch_to_task(
        &mut self,
        previous_task: Option<Arc<Task>>,
        next_task: Arc<Task>,
    ) -> Result<()> {
        let now_inst = now().expect("System timer not initialised");
        // Update the virtual clock before we do any other accounting.
        self.advance_vclock(now_inst);

        if let Some(ref prev_task) = previous_task {
            *prev_task.last_run.lock_save_irq() = Some(now_inst);
        }

        if let Some(ref prev_task) = previous_task
            && Arc::ptr_eq(&next_task, prev_task)
        {
            // Ensure the task state is running.
            *next_task.state.lock_save_irq() = TaskState::Running;
            // TODO: Fix hack
            if next_task.is_idle_task() {
                schedule_force_preempt();
            }
            return Ok(());
        }

        // Update vruntime, clear exec_start and assign a new eligible virtual deadline
        // for the previous task.
        if let Some(ref prev_task) = previous_task {
            // Compute how much virtual time the task actually consumed.
            let delta_vt = if let Some(start) = *prev_task.exec_start.lock_save_irq() {
                let delta = now_inst - start;
                let w = prev_task.weight() as u128;
                let dv = ((delta.as_nanos() as u128) << VT_FIXED_SHIFT) / w;
                *prev_task.v_runtime.lock_save_irq() += dv;
                dv
            } else {
                0
            };
            *prev_task.exec_start.lock_save_irq() = None;

            // Advance its eligible time by the virtual run time it just used
            // (EEVDF: v_ei += t_used / w_i).
            *prev_task.v_eligible.lock_save_irq() += delta_vt;

            // Re-issue a virtual deadline
            let q_ns: u128 = (DEFAULT_TIME_SLICE_MS as u128) * 1_000_000;
            let v_delta = (q_ns << VT_FIXED_SHIFT) / prev_task.weight() as u128;
            let v_ei = *prev_task.v_eligible.lock_save_irq();
            *prev_task.v_deadline.lock_save_irq() = v_ei + v_delta;
        }

        *next_task.exec_start.lock_save_irq() = Some(now_inst);
        *next_task.last_cpu.lock_save_irq() = CpuId::this();

        // Make sure the task possesses an eligible virtual deadline. If none is set
        // (or the previous one has elapsed), we hand out a brand-new one.
        {
            let mut deadline_guard = next_task.deadline.lock_save_irq();
            // Refresh deadline if none is set or the previous deadline has elapsed.
            if deadline_guard
                .is_none_or(|d| d <= now_inst + Duration::from_millis(DEFAULT_TIME_SLICE_MS))
            {
                *deadline_guard = Some(now_inst + Duration::from_millis(DEFAULT_TIME_SLICE_MS));
            }
            if let Some(d) = *deadline_guard {
                // log::debug!(
                //     "CPU {}: Next task {} has deadline in {}ms",
                //     CpuId::this().value(),
                //     next_task.tid.value(),
                //     (d - now_inst).as_millis()
                // );
                schedule_preempt(d);
            }
        }

        *next_task.state.lock_save_irq() = TaskState::Running;

        // Update the scheduler's state to reflect the new running task.
        self.running_task = Some(next_task.clone());

        // Perform the architecture-specific context switch.
        ArchImpl::context_switch(next_task);

        Ok(())
    }

    fn find_next_runnable_task(&self) -> Arc<Task> {
        let idle_task = self
            .run_queue
            .get(&TaskDescriptor::this_cpus_idle())
            .expect("Every runqueue should have an idle task");

        self.run_queue
            .values()
            // We only care about processes that are ready to run.
            .filter(|candidate_proc| {
                let state = *candidate_proc.state.lock_save_irq();
                let eligible_vt = *candidate_proc.v_eligible.lock_save_irq();
                state == TaskState::Runnable
                    && !candidate_proc.is_idle_task()
                    // Allow a small epsilon tolerance to compensate for rounding
                    && eligible_vt.saturating_sub(self.vclock) <= VCLOCK_EPSILON
            })
            .min_by(|proc1, proc2| {
                if proc1.is_idle_task() {
                    return Ordering::Greater;
                } else if proc2.is_idle_task() {
                    return Ordering::Less;
                }
                let vd1 = *proc1.v_deadline.lock_save_irq();
                let vd2 = *proc2.v_deadline.lock_save_irq();

                vd1.cmp(&vd2).then_with(|| {
                    let vr1 = *proc1.v_runtime.lock_save_irq();
                    let vr2 = *proc2.v_runtime.lock_save_irq();

                    vr1.cmp(&vr2).then_with(|| {
                        let last_run1 = proc1.last_run.lock_save_irq();
                        let last_run2 = proc2.last_run.lock_save_irq();

                        match (*last_run1, *last_run2) {
                            (Some(t1), Some(t2)) => t1.cmp(&t2),
                            (Some(_), None) => Ordering::Less,
                            (None, Some(_)) => Ordering::Greater,
                            (None, None) => Ordering::Equal,
                        }
                    })
                })
            })
            .unwrap_or(idle_task)
            .clone()
    }
}

pub fn current_task() -> Arc<Task> {
    SCHED_STATE
        .borrow()
        .running_task
        .as_ref()
        .expect("Current task called before initial task created")
        .clone()
}

pub fn sched_init() {
    let idle_task = get_idle_task();
    let init_task = Arc::new(Task::create_init_task());

    init_task
        .vm
        .lock_save_irq()
        .mm_mut()
        .address_space_mut()
        .activate();

    *init_task.state.lock_save_irq() = TaskState::Running;
    SCHED_STATE.borrow_mut().running_task = Some(idle_task.clone());

    {
        let mut task_list = TASK_LIST.lock_save_irq();

        task_list.insert(idle_task.descriptor(), Arc::downgrade(&idle_task.state));
        task_list.insert(init_task.descriptor(), Arc::downgrade(&init_task.state));
    }

    insert_task(idle_task);
    insert_task(init_task.clone());

    SCHED_STATE
        .borrow_mut()
        .switch_to_task(None, init_task)
        .expect("Failed to switch to init task");
}

pub fn sched_init_secondary() {
    let idle_task = get_idle_task();
    SCHED_STATE.borrow_mut().running_task = Some(idle_task.clone());

    // Important to ensure that the idle task is in the TASK_LIST for this CPU.
    insert_task(idle_task.clone());

    SCHED_STATE
        .borrow_mut()
        .switch_to_task(None, idle_task)
        .expect("Failed to switch to idle task");
}

fn get_idle_task() -> Arc<Task> {
    static IDLE_TASK: OnceLock<Arc<Task>> = OnceLock::new();

    IDLE_TASK
        .get_or_init(|| Arc::new(ArchImpl::create_idle_task()))
        .clone()
}

pub fn sys_sched_yield() -> Result<usize> {
    schedule();
    Ok(0)
}
