use crate::{
    interrupts::cpu_messenger::{Message, message_cpu},
    kernel::cpu_id::CpuId,
    process::{TASK_LIST, TaskDescriptor, TaskState},
};
use core::task::{RawWaker, RawWakerVTable, Waker};

use super::SCHED_STATE;

unsafe fn clone_waker(data: *const ()) -> RawWaker {
    RawWaker::new(data, &VTABLE)
}

/// Wakes the task. This consumes the waker.
unsafe fn wake_waker(data: *const ()) {
    let desc = TaskDescriptor::from_ptr(data);

    let task = TASK_LIST
        .lock_save_irq()
        .get(&desc)
        .and_then(|x| x.upgrade());

    if let Some(task) = task {
        let mut state = task.state.lock_save_irq();
        let locus = *task.last_cpu.lock_save_irq();

        match *state {
            // If the task has been put to sleep, then wake it up.
            TaskState::Sleeping | TaskState::Stopped => {
                if locus == CpuId::this() {
                    *state = TaskState::Runnable;
                    SCHED_STATE.borrow_mut().wakeup(desc);
                } else {
                    message_cpu(locus, Message::WakeupTask(create_waker(desc)))
                        .expect("Could not wakeup task on other CPU");
                }
            }
            // If the task is running, mark it so it doesn't actually go to
            // sleep when poll returns. This covers the small race-window
            // between a future returning `Poll::Pending` and the sched setting
            // the state to sleeping.
            TaskState::Running => {
                *state = TaskState::Woken;
            }
            _ => {}
        }
    }
}

unsafe fn drop_waker(_data: *const ()) {
    // There is nothing to do.
}

static VTABLE: RawWakerVTable =
    RawWakerVTable::new(clone_waker, wake_waker, wake_waker, drop_waker);

/// Creates a `Waker` for a given `Pid`.
pub fn create_waker(desc: TaskDescriptor) -> Waker {
    let raw_waker = RawWaker::new(desc.to_ptr(), &VTABLE);

    // SAFETY: We have correctly implemented the VTable functions.
    unsafe { Waker::from_raw(raw_waker) }
}
