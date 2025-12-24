use crate::drivers::timer::now;
use crate::{
    arch::{Arch, ArchImpl},
    per_cpu,
    process::{TASK_LIST, Task, TaskDescriptor, TaskState},
    sync::OnceLock,
};
use alloc::{boxed::Box, collections::btree_map::BTreeMap, sync::Arc};
use core::cmp::Ordering;
use libkernel::{UserAddressSpace, error::Result};

pub mod uspc_ret;
pub mod waker;

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
///    -&gt; `Runnable` for the old task, `Runnable` -&gt; `Running` for the new task)
///    and performs the architecture-specific context switch.
///
/// # Returns
///
/// Nothing, but the CPU context will be set to the next runnable task. See
/// `userspace_return` for how this is invoked.
fn schedule() {
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
    let next_task = sched_state.find_next_runnable_task();

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

/// Insert the given task onto *this* CPUs runqueue.
pub fn insert_task(task: Arc<Task>) {
    SCHED_STATE
        .borrow_mut()
        .run_queue
        .insert(task.descriptor(), task);
}

pub struct SchedState {
    running_task: Option<Arc<Task>>,
    pub run_queue: BTreeMap<TaskDescriptor, Arc<Task>>,
}

unsafe impl Send for SchedState {}

impl SchedState {
    pub const fn new() -> Self {
        Self {
            running_task: None,
            run_queue: BTreeMap::new(),
        }
    }

    fn switch_to_task(
        &mut self,
        previous_task: Option<Arc<Task>>,
        next_task: Arc<Task>,
    ) -> Result<()> {
        let now_inst = now().expect("System timer not initialised");

        if let Some(ref prev_task) = previous_task {
            *prev_task.last_run.lock_save_irq() = Some(now_inst);
        }

        if let Some(ref prev_task) = previous_task
            && Arc::ptr_eq(&next_task, prev_task)
        {
            // Ensure the task state is running.
            *next_task.state.lock_save_irq() = TaskState::Running;
            return Ok(());
        }

        // Update vruntime and clear exec_start for the previous task.
        if let Some(ref prev_task) = previous_task {
            if let Some(start) = *prev_task.exec_start.lock_save_irq() {
                let delta = now_inst - start;
                *prev_task.vruntime.lock_save_irq() += delta.as_nanos() as u64;
            }
            *prev_task.exec_start.lock_save_irq() = None;
        }

        // Record the start time for the task we are about to run.
        *next_task.exec_start.lock_save_irq() = Some(now_inst);

        // Context switch, the previous task's state should already been updated
        // prior to calling this function.
        if let Some(ref prev_task) = previous_task {
            debug_assert_eq!(*prev_task.state.lock_save_irq(), TaskState::Runnable);
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
                // A process is a candidate if it's runnable and NOT the idle task
                state == TaskState::Runnable && !candidate_proc.is_idle_task()
            })
            .min_by(|proc1, proc2| {
                let vr1 = *proc1.vruntime.lock_save_irq();
                let vr2 = *proc2.vruntime.lock_save_irq();

                vr1.cmp(&vr2).then_with(|| {
                    // Tie-breaker: fall back to last run timestamp.
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
            .unwrap_or(idle_task)
            .clone()
    }
}

per_cpu! {
    pub static SCHED_STATE: SchedState = SchedState::new;
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

    insert_task(idle_task.clone());

    SCHED_STATE
        .borrow_mut()
        .switch_to_task(None, idle_task)
        .expect("Failed to swtich to idle task");
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
