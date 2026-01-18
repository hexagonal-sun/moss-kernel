use alloc::collections::VecDeque;
use core::cell::UnsafeCell;
use core::future::Future;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use core::task::{Context, Poll, Waker};

use crate::CpuOps;

use super::spinlock::SpinLockIrq;

struct RwlockState<CPU: CpuOps> {
    lock_state: AtomicI32,
    read_waiters: SpinLockIrq<VecDeque<Waker>, CPU>,
    write_waiters: SpinLockIrq<VecDeque<Waker>, CPU>,
    last_woken_was_writer: AtomicBool,
}

/// An asynchronous, mutex primitive.
///
/// This mutex can be used to protect shared data across asynchronous tasks.
/// `lock()` returns a future that resolves to a guard. When the guard is
/// dropped, the lock is released.
pub struct Rwlock<T: ?Sized, CPU: CpuOps> {
    state: RwlockState<CPU>,
    data: UnsafeCell<T>,
}

/// A guard that provides read-only access to the data in an `AsyncRwlock`.
///
/// When an `AsyncRwlockReadGuard` is dropped, it automatically decreases the
/// read count and wakes up the next task if necessary.
#[must_use = "if unused, the Rwlock will immediately unlock"]
pub struct AsyncRwlockReadGuard<'a, T: ?Sized, CPU: CpuOps> {
    rwlock: &'a Rwlock<T, CPU>,
}

/// A future that resolves to an `AsyncRwlockReadGuard` when the lock is acquired.
pub struct RwlockReadGuardFuture<'a, T: ?Sized, CPU: CpuOps> {
    rwlock: &'a Rwlock<T, CPU>,
}

/// A guard that provides exclusive access to the data in an `AsyncRwlock`.
///
/// When an `AsyncRwlockWriteGuard` is dropped, it automatically releases the lock and
/// wakes up the next task.
#[must_use = "if unused, the Rwlock will immediately unlock"]
pub struct AsyncRwlockWriteGuard<'a, T: ?Sized, CPU: CpuOps> {
    rwlock: &'a Rwlock<T, CPU>,
}

/// A future that resolves to an `AsyncRwlockWriteGuard` when the lock is acquired.
pub struct RwlockWriteGuardFuture<'a, T: ?Sized, CPU: CpuOps> {
    rwlock: &'a Rwlock<T, CPU>,
}

impl<T, CPU: CpuOps> Rwlock<T, CPU> {
    /// Creates a new asynchronous mutex in an unlocked state.
    pub const fn new(data: T) -> Self {
        Self {
            state: RwlockState {
                lock_state: AtomicI32::new(0),
                read_waiters: SpinLockIrq::new(VecDeque::new()),
                write_waiters: SpinLockIrq::new(VecDeque::new()),
                last_woken_was_writer: AtomicBool::new(false),
            },
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

impl<T: ?Sized, CPU: CpuOps> Rwlock<T, CPU> {
    /// Acquires rwlock read.
    ///
    /// Returns a future that resolves to a lock guard. The returned future must
    /// be `.await`ed to acquire the read guard. The guard is released when the
    /// returned [`AsyncRwlockReadGuard`] is dropped.
    pub fn read(&self) -> RwlockReadGuardFuture<'_, T, CPU> {
        RwlockReadGuardFuture { rwlock: self }
    }

    /// Acquires rwlock write.
    ///
    /// Returns a future that resolves to a lock guard. The returned future must
    /// be `.await`ed to acquire the write guard. The guard is released when the
    /// returned [`AsyncRwlockWriteGuard`] is dropped.
    pub fn write(&self) -> RwlockWriteGuardFuture<'_, T, CPU> {
        RwlockWriteGuardFuture { rwlock: self }
    }
}

impl<'a, T: ?Sized, CPU: CpuOps> Future for RwlockReadGuardFuture<'a, T, CPU> {
    type Output = AsyncRwlockReadGuard<'a, T, CPU>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rwlock.state.lock_state.load(Ordering::Acquire) {
            0.. => {
                self.rwlock.state.lock_state.fetch_add(1, Ordering::AcqRel);
                Poll::Ready(AsyncRwlockReadGuard {
                    rwlock: self.rwlock,
                })
            }
            -1 => {
                let mut read_waiters = self.rwlock.state.read_waiters.lock_save_irq();
                if read_waiters.iter().all(|w| !w.will_wake(cx.waker())) {
                    read_waiters.push_back(cx.waker().clone());
                }
                Poll::Pending
            }
            _ => unreachable!(),
        }
    }
}

impl<T: ?Sized, CPU: CpuOps> Drop for AsyncRwlockReadGuard<'_, T, CPU> {
    fn drop(&mut self) {
        match self.rwlock.state.lock_state.load(Ordering::Acquire) {
            2.. => {
                self.rwlock.state.lock_state.fetch_sub(1, Ordering::AcqRel);
            }
            1 => {
                self.rwlock.state.lock_state.store(0, Ordering::Release);
                let write_waiters = &mut self.rwlock.state.write_waiters.lock_save_irq();
                if let Some(next_waker) = write_waiters.pop_front() {
                    next_waker.wake();
                }
            }
            _ => unreachable!(),
        }
    }
}

impl<T: ?Sized, CPU: CpuOps> Deref for AsyncRwlockReadGuard<'_, T, CPU> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: This is safe because the existence of this guard guarantees
        // we have exclusive access to the data.
        unsafe { &*self.rwlock.data.get() }
    }
}

impl<'a, T: ?Sized, CPU: CpuOps> Future for RwlockWriteGuardFuture<'a, T, CPU> {
    type Output = AsyncRwlockWriteGuard<'a, T, CPU>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.rwlock.state.lock_state.load(Ordering::Acquire) {
            0 => {
                self.rwlock.state.lock_state.store(-1, Ordering::Release);
                Poll::Ready(AsyncRwlockWriteGuard {
                    rwlock: self.rwlock,
                })
            }
            _ => {
                let mut write_waiters = self.rwlock.state.write_waiters.lock_save_irq();
                if write_waiters.iter().all(|w| !w.will_wake(cx.waker())) {
                    write_waiters.push_back(cx.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

impl<T: ?Sized, CPU: CpuOps> Drop for AsyncRwlockWriteGuard<'_, T, CPU> {
    fn drop(&mut self) {
        // Alternate between waking readers and writers to prevent starvation.
        let was_writer = self
            .rwlock
            .state
            .last_woken_was_writer
            .load(Ordering::Acquire);
        self.rwlock
            .state
            .last_woken_was_writer
            .store(!was_writer, Ordering::Release);
        self.rwlock.state.lock_state.store(0, Ordering::Release);
        let read_waiters = &mut self.rwlock.state.read_waiters.lock_save_irq();
        let write_waiters = &mut self.rwlock.state.write_waiters.lock_save_irq();
        if (was_writer && !read_waiters.is_empty()) || write_waiters.is_empty() {
            while let Some(next_waker) = read_waiters.pop_front() {
                next_waker.wake();
            }
        } else if let Some(next_waker) = write_waiters.pop_front() {
            next_waker.wake();
        }
    }
}

impl<T: ?Sized, CPU: CpuOps> Deref for AsyncRwlockWriteGuard<'_, T, CPU> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: This is safe because the existence of this guard guarantees
        // we have exclusive access to the data.
        unsafe { &*self.rwlock.data.get() }
    }
}

impl<T: ?Sized, CPU: CpuOps> DerefMut for AsyncRwlockWriteGuard<'_, T, CPU> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: This is safe because the existence of this guard guarantees
        // we have exclusive access to the data.
        unsafe { &mut *self.rwlock.data.get() }
    }
}

unsafe impl<T: ?Sized + Send, CPU: CpuOps> Send for Rwlock<T, CPU> {}
unsafe impl<T: ?Sized + Send, CPU: CpuOps> Sync for Rwlock<T, CPU> {}
