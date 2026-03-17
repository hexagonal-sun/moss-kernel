use crate::process::{Task, owned::OwnedTask};
use alloc::sync::Arc;

/// Provides access to the current task's state.
///
/// Any function that is marked with `ProcessCtx` should only be callable from a
/// context which is backed-by a userspace context. As such, they should take a
/// `ProcessCtx` as an argument to enforce this requirement. A new `ProcessCtx`
/// is created by the arch layer following entry into the kernel from a process
/// context and it is passed to relevent functions.
pub struct ProcessCtx {
    task: *mut OwnedTask,
}

// Safety: The kernel guarantees that an OwnedTask is only accessed by one CPU
// at a time.
unsafe impl Send for ProcessCtx {}

// Safety: The kernel guarantees that an OwnedTask is only accessed by one CPU
// at a time.
unsafe impl Sync for ProcessCtx {}

impl ProcessCtx {
    /// Create a `ProcessCtx` from a raw pointer to the current task.
    ///
    /// # Safety
    ///
    /// - `task` must point to a valid, heap-allocated `OwnedTask` that will
    ///   remain alive for the lifetime of this `ProcessCtx`.
    /// - The caller must ensure single-CPU access: no other mutable references
    ///   to the `OwnedTask` may exist concurrently.
    pub unsafe fn new(task: *mut OwnedTask) -> Self {
        debug_assert!(!task.is_null());
        Self { task }
    }

    /// Create a `ProcessCtx` for the currently-running task on this CPU.
    ///
    /// Obtains the raw pointer from the scheduler's `current_work()`.
    ///
    /// # Safety
    ///
    /// The caller must ensure single-CPU access: no other mutable references to
    /// the `OwnedTask` may exist concurrently. Furthermore, the caller must
    /// ensure that the kernel has been entered when in a process ctx, for
    /// example when handling a synchronous exception from userspace.
    pub unsafe fn from_current() -> Self {
        let work = super::current_work();
        unsafe { Self::new(alloc::boxed::Box::as_ptr(&work.task) as *mut _) }
    }

    /// Shared access to the CPU-local owned task.
    pub fn task(&self) -> &OwnedTask {
        unsafe { &*self.task }
    }

    /// Create a new `SyscallCtx` pointing to the same task.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the cloned value and `self` aren't used
    /// concurrently.
    pub unsafe fn clone(&self) -> Self {
        unsafe { Self::new(self.task) }
    }

    pub fn task_mut(&mut self) -> &mut OwnedTask {
        unsafe { &mut *self.task }
    }

    pub fn shared(&self) -> &Arc<Task> {
        &self.task().t_shared
    }
}
