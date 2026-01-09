use alloc::collections::VecDeque;
use core::cell::UnsafeCell;
use core::future::Future;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::CpuOps;

use super::spinlock::SpinLockIrq;

struct MutexState {
    is_locked: bool,
    waiters: VecDeque<Waker>,
}

/// An asynchronous, mutex primitive.
///
/// This mutex can be used to protect shared data across asynchronous tasks.
/// `lock()` returns a future that resolves to a guard. When the guard is
/// dropped, the lock is released.
pub struct Mutex<T: ?Sized, CPU: CpuOps> {
    state: SpinLockIrq<MutexState, CPU>,
    data: UnsafeCell<T>,
}

/// A guard that provides exclusive access to the data in an `AsyncMutex`.
///
/// When an `AsyncMutexGuard` is dropped, it automatically releases the lock and
/// wakes up the next task.
#[must_use = "if unused, the Mutex will immediately unlock"]
pub struct AsyncMutexGuard<'a, T: ?Sized, CPU: CpuOps> {
    mutex: &'a Mutex<T, CPU>,
}

/// A future that resolves to an `AsyncMutexGuard` when the lock is acquired.
pub struct MutexGuardFuture<'a, T: ?Sized, CPU: CpuOps> {
    mutex: &'a Mutex<T, CPU>,
}

impl<T, CPU: CpuOps> Mutex<T, CPU> {
    /// Creates a new asynchronous mutex in an unlocked state.
    pub const fn new(data: T) -> Self {
        Self {
            state: SpinLockIrq::new(MutexState {
                is_locked: false,
                waiters: VecDeque::new(),
            }),
            data: UnsafeCell::new(data),
        }
    }

    /// Consumes the mutex, returning the underlying data.
    ///
    /// This is safe because consuming `self` guarantees no other code can
    /// access the mutex.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized, CPU: CpuOps> Mutex<T, CPU> {
    /// Acquires the mutex lock.
    ///
    /// Returns a future that resolves to a lock guard. The returned future must
    /// be `.await`ed to acquire the lock. The lock is released when the
    /// returned `AsyncMutexGuard` is dropped.
    pub fn lock(&self) -> MutexGuardFuture<'_, T, CPU> {
        MutexGuardFuture { mutex: self }
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place - the mutable borrow statically guarantees that no other
    /// references to the `Mutex` exist.
    pub fn get_mut(&mut self) -> &mut T {
        // SAFETY: We can grant mutable access to the data because `&mut self`
        // guarantees that no other threads are concurrently accessing the
        // mutex. No other code can call `.lock()` because we hold the unique
        // mutable reference. Thus, we can safely bypass the lock.
        unsafe { &mut *self.data.get() }
    }
}

impl<'a, T: ?Sized, CPU: CpuOps> Future for MutexGuardFuture<'a, T, CPU> {
    type Output = AsyncMutexGuard<'a, T, CPU>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.mutex.state.lock_save_irq();

        if !state.is_locked {
            state.is_locked = true;
            Poll::Ready(AsyncMutexGuard { mutex: self.mutex })
        } else {
            if state.waiters.iter().all(|w| !w.will_wake(cx.waker())) {
                state.waiters.push_back(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}

impl<T: ?Sized, CPU: CpuOps> Drop for AsyncMutexGuard<'_, T, CPU> {
    fn drop(&mut self) {
        let mut state = self.mutex.state.lock_save_irq();

        if let Some(next_waker) = state.waiters.pop_front() {
            next_waker.wake();
        }

        state.is_locked = false;
    }
}

impl<T: ?Sized, CPU: CpuOps> Deref for AsyncMutexGuard<'_, T, CPU> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: This is safe because the existence of this guard guarantees
        // we have exclusive access to the data.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized, CPU: CpuOps> DerefMut for AsyncMutexGuard<'_, T, CPU> {
    fn deref_mut(&mut self) -> &mut T {
        // This is safe for the same reason.
        unsafe { &mut *self.mutex.data.get() }
    }
}

unsafe impl<T: ?Sized + Send, CPU: CpuOps> Send for Mutex<T, CPU> {}
unsafe impl<T: ?Sized + Send, CPU: CpuOps> Sync for Mutex<T, CPU> {}

impl<CPU: CpuOps> Mutex<(), CPU> {
    /// Acquires the mutex lock without caring about the data.
    pub(crate) fn acquire(&self) -> MutexAcquireFuture<'_, CPU> {
        MutexAcquireFuture { mutex: self }
    }

    /// Releases the mutex lock without caring about the data.
    ///
    /// # Safety
    /// The caller must ensure that they have previously called [`Self::acquire()`].
    pub(crate) unsafe fn release(&self) {
        let mut state = self.state.lock_save_irq();

        if let Some(next_waker) = state.waiters.pop_front() {
            next_waker.wake();
        }

        state.is_locked = false;
    }
}

/// A future that resolves to a locked mutex
pub struct MutexAcquireFuture<'a, CPU: CpuOps> {
    mutex: &'a Mutex<(), CPU>,
}

impl<CPU: CpuOps> Future for MutexAcquireFuture<'_, CPU> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.mutex.state.lock_save_irq();

        if !state.is_locked {
            state.is_locked = true;
            Poll::Ready(())
        } else {
            if state.waiters.iter().all(|w| !w.will_wake(cx.waker())) {
                state.waiters.push_back(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}
