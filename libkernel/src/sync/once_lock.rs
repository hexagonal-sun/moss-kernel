//! A thread-safe cell that is initialized exactly once.

use core::fmt;

use crate::CpuOps;

use super::spinlock::SpinLockIrq;

/// A cell which can be written to only once.
///
/// This is a kernel-safe, no_std equivalent of std::sync::OnceLock, built on
/// top of a SpinLock.
pub struct OnceLock<T, CPU: CpuOps> {
    inner: SpinLockIrq<Option<T>, CPU>,
}

impl<T, CPU: CpuOps> OnceLock<T, CPU> {
    /// Creates a new, empty `OnceLock`.
    pub const fn new() -> Self {
        OnceLock {
            inner: SpinLockIrq::new(None),
        }
    }

    /// Gets a reference to the contained value, if it has been initialized.
    pub fn get(&self) -> Option<&T> {
        let guard = self.inner.lock_save_irq();

        if let Some(value) = guard.as_ref() {
            // SAFETY: This is the only `unsafe` part. We are "extending" the
            // lifetime of the reference beyond the scope of the lock guard.
            //
            // This is sound because we guarantee that once the `Option<T>` is
            // `Some(T)`, it will *never* be changed back to `None` or to a
            // different `Some(T)`. The value is stable in memory for the
            // lifetime of the `OnceLock` itself.
            let ptr: *const T = value;
            Some(unsafe { &*ptr })
        } else {
            None
        }
    }

    /// Gets a mutable reference to the contained value, if it has been
    /// initialized.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        let mut guard = self.inner.lock_save_irq();

        if let Some(value) = guard.as_mut() {
            // SAFETY: This is the only `unsafe` part. We are "extending" the
            // lifetime of the reference beyond the scope of the lock guard.
            //
            // This is sound because we guarantee that once the `Option<T>` is
            // `Some(T)`, it will *never* be changed back to `None` or to a
            // different `Some(T)`. The value is stable in memory for the
            // lifetime of the `OnceLock` itself.
            let ptr: *mut T = value;
            Some(unsafe { &mut *ptr })
        } else {
            None
        }
    }

    /// Gets the contained value, or initializes it with a closure if it is empty.
    pub fn get_or_init<F>(&self, f: F) -> &T
    where
        F: FnOnce() -> T,
    {
        if let Some(value) = self.get() {
            return value;
        }

        // The value was not initialized. We need to acquire a full lock
        // to potentially initialize it.
        self.initialize(f)
    }

    #[cold]
    fn initialize<F>(&self, f: F) -> &T
    where
        F: FnOnce() -> T,
    {
        let mut guard = self.inner.lock_save_irq();

        // We must check again! Between our `get()` call and acquiring the lock,
        // another core could have initialized the value. If we don't check
        // again, we would initialize it a second time.
        let value = match *guard {
            Some(ref value) => value,
            None => {
                // It's still None, so we are the first. We run the closure
                // and set the value.
                let new_value = f();
                guard.insert(new_value) // `insert` places the value and returns a &mut to it
            }
        };

        // As before, we can now safely extend the lifetime of the reference.
        let ptr: *const T = value;
        unsafe { &*ptr }
    }

    /// Attempts to set the value of the `OnceLock`.
    ///
    /// If the cell is already initialized, the given value is returned in an
    /// `Err`. This is useful for when initialization might fail and you don't
    /// want to use a closure-based approach.
    pub fn set(&self, value: T) -> Result<(), T> {
        let mut guard = self.inner.lock_save_irq();
        if guard.is_some() {
            Err(value)
        } else {
            *guard = Some(value);
            Ok(())
        }
    }
}

impl<T, CPU: CpuOps> Default for OnceLock<T, CPU> {
    fn default() -> Self {
        Self::new()
    }
}

// Implement Debug for nice printing, if the inner type supports it.
impl<T: fmt::Debug, CPU: CpuOps> fmt::Debug for OnceLock<T, CPU> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnceLock")
            .field("inner", &self.get())
            .finish()
    }
}

unsafe impl<T: Sync + Send, CPU: CpuOps> Sync for OnceLock<T, CPU> {}
unsafe impl<T: Send, CPU: CpuOps> Send for OnceLock<T, CPU> {}
