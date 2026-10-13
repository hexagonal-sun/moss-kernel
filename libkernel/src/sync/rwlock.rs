//! Async-aware readers–writer lock.

use super::{spinlock::SpinLockIrq, waker_set::WakerSet};
use crate::CpuOps;
use core::cell::UnsafeCell;
use core::future::Future;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

struct RwlockState {
    num_readers: usize,
    writer_active: bool,
    waiting_writers: usize,
    read_waiters: WakerSet,
    write_waiters: WakerSet,
}

// Wakers may re-enter the lock, so extract notifications under the spinlock
// but run their callbacks only after releasing it and restoring interrupts.
enum Wakeup {
    None,
    Writer(Waker),
    Readers(WakerSet),
}

impl Wakeup {
    fn wake(self) {
        match self {
            Self::None => {}
            Self::Writer(waker) => waker.wake(),
            Self::Readers(mut waiters) => waiters.wake_all(),
        }
    }
}

impl RwlockState {
    fn take_waiters(&mut self) -> Wakeup {
        if self.writer_active {
            return Wakeup::None;
        }

        if self.waiting_writers != 0 {
            if self.num_readers == 0
                && let Some((waker, ())) = self.write_waiters.take_first()
            {
                return Wakeup::Writer(waker);
            }
            Wakeup::None
        } else {
            Wakeup::Readers(self.read_waiters.take_all())
        }
    }
}

// Pending acquisitions own only a waiter registration, never a lock or a
// reader slot. A writer remains counted even after its waker has been removed
// by a notification, until it acquires the lock or is cancelled.
struct AcquireFuture<'a, T: ?Sized, CPU: CpuOps> {
    rwlock: &'a Rwlock<T, CPU>,
    write: bool,
    token: Option<u64>,
}

impl<T: ?Sized, CPU: CpuOps> Future for AcquireFuture<'_, T, CPU> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let waker = cx.waker().clone();
        let mut state = this.rwlock.state.lock_save_irq();
        let can_acquire = !state.writer_active
            && if this.write {
                state.num_readers == 0
            } else {
                state.waiting_writers == 0
            };

        if can_acquire {
            let retired = this.token.take().and_then(|token| {
                if this.write {
                    state.waiting_writers -= 1;
                    state.write_waiters.take(token)
                } else {
                    state.read_waiters.take(token)
                }
            });

            if this.write {
                state.writer_active = true;
            } else {
                state.num_readers = state
                    .num_readers
                    .checked_add(1)
                    .expect("rwlock reader count overflow");
            }
            drop(state);
            drop(retired);
            return Poll::Ready(());
        }

        // Refresh the waker on every pending poll. Notifications remove
        // registrations, and another task may take the lock before we run.
        let retired = if this.write {
            if let Some(token) = this.token {
                state.write_waiters.take(token)
            } else {
                state.waiting_writers = state
                    .waiting_writers
                    .checked_add(1)
                    .expect("rwlock waiting writer count overflow");
                None
            }
        } else {
            this.token.and_then(|token| state.read_waiters.take(token))
        };
        let waiters = if this.write {
            &mut state.write_waiters
        } else {
            &mut state.read_waiters
        };
        this.token = Some(waiters.insert(waker, ()));
        drop(state);
        drop(retired);

        Poll::Pending
    }
}

impl<T: ?Sized, CPU: CpuOps> Drop for AcquireFuture<'_, T, CPU> {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            let mut state = self.rwlock.state.lock_save_irq();
            let retired = if self.write {
                state.waiting_writers -= 1;
                state.write_waiters.take(token)
            } else {
                state.read_waiters.take(token)
            };
            // Cancellation of a queued or already-notified writer must not
            // leave other waiters asleep while the lock is available.
            let wakeup = state.take_waiters();
            drop(state);
            drop(retired);
            wakeup.wake();
        }
    }
}

/// An asynchronous, rwlock primitive.
///
/// This rwlock can be used to protect shared data across asynchronous tasks.
/// [`read()`](Self::read) and [`write()`](Self::write) return futures that resolve
/// to guards. When a guard is dropped, its lock is released.
///
/// Waiting writers take precedence over new readers.
pub struct Rwlock<T: ?Sized, CPU: CpuOps> {
    state: SpinLockIrq<RwlockState, CPU>,
    data: UnsafeCell<T>,
}

/// A guard that provides read-only access to the data in a [`Rwlock`].
///
/// When an `AsyncRwlockReadGuard` is dropped, it automatically decreases the
/// read count and wakes up the next task if necessary.
#[must_use = "if unused, the Rwlock will immediately unlock"]
pub struct AsyncRwlockReadGuard<'a, T: ?Sized, CPU: CpuOps> {
    rwlock: &'a Rwlock<T, CPU>,
}

/// A guard that provides exclusive access to the data in a [`Rwlock`].
///
/// When an `AsyncRwlockWriteGuard` is dropped, it automatically releases the lock
/// and wakes up the next task.
#[must_use = "if unused, the Rwlock will immediately unlock"]
pub struct AsyncRwlockWriteGuard<'a, T: ?Sized, CPU: CpuOps> {
    rwlock: &'a Rwlock<T, CPU>,
}

impl<T, CPU: CpuOps> Rwlock<T, CPU> {
    /// Creates a new asynchronous rwlock in an unlocked state.
    pub fn new(data: T) -> Self {
        Self {
            state: SpinLockIrq::new(RwlockState {
                num_readers: 0,
                writer_active: false,
                waiting_writers: 0,
                read_waiters: WakerSet::new(),
                write_waiters: WakerSet::new(),
            }),
            data: UnsafeCell::new(data),
        }
    }

    /// Consumes the rwlock, returning the underlying data.
    ///
    /// This is safe because consuming `self` guarantees no other code can
    /// access the rwlock.
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized, CPU: CpuOps> Rwlock<T, CPU> {
    /// Acquires rwlock read.
    ///
    /// Returns a guard asynchronously. The guard is released when the
    /// returned [`AsyncRwlockReadGuard`] is dropped.
    pub async fn read(&self) -> AsyncRwlockReadGuard<'_, T, CPU> {
        AcquireFuture {
            rwlock: self,
            write: false,
            token: None,
        }
        .await;
        AsyncRwlockReadGuard { rwlock: self }
    }

    /// Acquires rwlock write.
    ///
    /// Returns a guard asynchronously. The guard is released when the
    /// returned [`AsyncRwlockWriteGuard`] is dropped.
    pub async fn write(&self) -> AsyncRwlockWriteGuard<'_, T, CPU> {
        AcquireFuture {
            rwlock: self,
            write: true,
            token: None,
        }
        .await;
        AsyncRwlockWriteGuard { rwlock: self }
    }
}

impl<T: ?Sized, CPU: CpuOps> Drop for AsyncRwlockReadGuard<'_, T, CPU> {
    fn drop(&mut self) {
        let mut state = self.rwlock.state.lock_save_irq();
        state.num_readers -= 1;
        let wakeup = state.take_waiters();
        drop(state);
        wakeup.wake();
    }
}

impl<T: ?Sized, CPU: CpuOps> Deref for AsyncRwlockReadGuard<'_, T, CPU> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: The existence of this guard guarantees read access without
        // any writers. Concurrent read access requires T: Sync to share a lock.
        unsafe { &*self.rwlock.data.get() }
    }
}

impl<T: ?Sized, CPU: CpuOps> Drop for AsyncRwlockWriteGuard<'_, T, CPU> {
    fn drop(&mut self) {
        let mut state = self.rwlock.state.lock_save_irq();
        state.writer_active = false;
        let wakeup = state.take_waiters();
        drop(state);
        wakeup.wake();
    }
}

impl<T: ?Sized, CPU: CpuOps> Deref for AsyncRwlockWriteGuard<'_, T, CPU> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: The existence of this guard guarantees exclusive access.
        unsafe { &*self.rwlock.data.get() }
    }
}

impl<T: ?Sized, CPU: CpuOps> DerefMut for AsyncRwlockWriteGuard<'_, T, CPU> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: The existence of this guard guarantees exclusive access.
        unsafe { &mut *self.rwlock.data.get() }
    }
}

// SAFETY: Moving an owned lock transfers ownership of T. Sharing a lock also
// permits concurrent readers to access &T, so T must be Sync as well as Send.
unsafe impl<T: ?Sized + Send, CPU: CpuOps> Send for Rwlock<T, CPU> {}
unsafe impl<T: ?Sized + Send + Sync, CPU: CpuOps> Sync for Rwlock<T, CPU> {}
