use alloc::sync::Arc;
use core::task::{RawWaker, RawWakerVTable, Waker};

use super::{
    insert_work_cross_cpu,
    sched_task::{Work, state::WakerAction},
};

unsafe fn clone_waker(data: *const ()) -> RawWaker {
    let data: *const Work = data.cast();

    unsafe { Arc::increment_strong_count(data) };

    RawWaker::new(data.cast(), &VTABLE)
}

/// Wakes the task. This does not consume the waker.
unsafe fn wake_waker_no_consume(data: *const ()) {
    let data: *const Work = data.cast();

    // Increment the strong count first so that Arc::from_raw does not
    // consume the waker's own reference.
    unsafe { Arc::increment_strong_count(data) };
    let work = unsafe { Arc::from_raw(data) };

    match work.state.wake() {
        WakerAction::Enqueue => {
            insert_work_cross_cpu(work);
        }
        WakerAction::PreventedSleep | WakerAction::None => {}
    }
}

unsafe fn wake_waker_consume(data: *const ()) {
    unsafe {
        wake_waker_no_consume(data);
        drop_waker(data);
    }
}

unsafe fn drop_waker(data: *const ()) {
    let data: *const Work = data.cast();
    unsafe { Arc::decrement_strong_count(data) };
}

static VTABLE: RawWakerVTable = RawWakerVTable::new(
    clone_waker,
    wake_waker_consume,
    wake_waker_no_consume,
    drop_waker,
);

/// Creates a `Waker` for a given `Pid`.
pub fn create_waker(work: Arc<Work>) -> Waker {
    let raw_waker = RawWaker::new(Arc::into_raw(work).cast(), &VTABLE);

    // SAFETY: We have correctly implemented the VTable functions.
    unsafe { Waker::from_raw(raw_waker) }
}
