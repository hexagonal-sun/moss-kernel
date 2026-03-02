use crate::{
    per_cpu_private,
    process::{Task, owned::OwnedTask},
};
use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::Cell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    ptr,
};

per_cpu_private! {
    pub(super) static CUR_TASK_PTR: CurrentTaskPtr = CurrentTaskPtr::new;
}

pub(super) struct CurrentTaskPtr {
    pub(super) ptr: Cell<*mut OwnedTask>,
    pub(super) borrowed: Cell<bool>,
    location: Cell<Option<core::panic::Location<'static>>>,
}

unsafe impl Send for CurrentTaskPtr {}

pub struct CurrentTaskGuard<'a> {
    task: &'a mut OwnedTask,
    _marker: PhantomData<*const ()>,
}

impl Deref for CurrentTaskGuard<'_> {
    type Target = OwnedTask;

    fn deref(&self) -> &Self::Target {
        self.task
    }
}

impl DerefMut for CurrentTaskGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.task
    }
}

impl<'a> Drop for CurrentTaskGuard<'a> {
    fn drop(&mut self) {
        let current = CUR_TASK_PTR.borrow();
        current.borrowed.set(false);
        current.location.set(None);
    }
}

impl CurrentTaskPtr {
    pub const fn new() -> Self {
        Self {
            ptr: Cell::new(ptr::null_mut()),
            borrowed: Cell::new(false),
            location: Cell::new(None),
        }
    }

    #[track_caller]
    pub fn current(&self) -> CurrentTaskGuard<'static> {
        if self.borrowed.get() {
            let other = self.location.take();
            panic!("Double mutable borrow of current task! Borrowed from: {other:?}");
        }

        self.borrowed.set(true);
        self.location.set(Some(*core::panic::Location::caller()));

        unsafe {
            let ptr = self.ptr.get();

            CurrentTaskGuard {
                task: &mut *ptr,
                _marker: PhantomData,
            }
        }
    }

    pub(super) fn set_current(&self, task: &mut Box<OwnedTask>) {
        self.ptr.set(Box::as_mut_ptr(task));
    }
}

/// Returns a mutable reference to the CPU-local private task state
/// (`OwnedTask`).
///
/// # Panics
///
/// Panics if the current task is already borrowed on this CPU (reentrancy bug).
/// This usually happens if you call `current_task()` and then call a function
/// that also calls `current_task()` without dropping the first guard.
///
/// # Critical Section
///
/// This function disables preemption. You must drop the returned guard before
/// attempting to sleep, yield, or await.
#[track_caller]
pub fn current_task() -> CurrentTaskGuard<'static> {
    CUR_TASK_PTR.borrow_mut().current()
}

/// Returns a shared reference to the Process Identity (`Task`).
///
/// Use this for accessing shared resources like:
/// - File Descriptors
/// - Virtual Memory (Page Tables)
/// - Current Working Directory
/// - Credentials / PID / Thread Group
///
/// # Execution Context
///
/// This function creates a temporary `CurrentTaskGuard` just long enough to
/// clone the `Arc`, then drops it. It is safe to await or yield after calling
/// this function, as it does not hold the CPU lock.
pub fn current_task_shared() -> Arc<Task> {
    current_task().t_shared.clone()
}
