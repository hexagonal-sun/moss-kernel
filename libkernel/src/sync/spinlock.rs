//! Low-level spin lock primitives.

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::CpuOps;

/// A spinlock that also disables interrupts on the local core while held.
///
/// This prevents deadlocks with interrupt handlers on the same core and
/// provides SMP-safety against other cores.
pub struct SpinLockIrq<T: ?Sized, CPU: CpuOps> {
    lock: AtomicBool,
    _phantom: PhantomData<CPU>,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send, CPU: CpuOps> Send for SpinLockIrq<T, CPU> {}
unsafe impl<T: ?Sized + Send, CPU: CpuOps> Sync for SpinLockIrq<T, CPU> {}

impl<T, CPU: CpuOps> SpinLockIrq<T, CPU> {
    /// Creates a new IRQ-safe spinlock.
    pub const fn new(data: T) -> Self {
        Self {
            lock: AtomicBool::new(false),
            _phantom: PhantomData,
            data: UnsafeCell::new(data),
        }
    }
}

impl<T: ?Sized, CPU: CpuOps> SpinLockIrq<T, CPU> {
    /// Disables interrupts, acquires the lock, and returns a guard. The
    /// original interrupt state is restored when the guard is dropped.
    pub fn lock_save_irq(&self) -> SpinLockIrqGuard<'_, T, CPU> {
        let saved_irq_flags = CPU::disable_interrupts();

        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Spin while waiting for the lock to become available.
            // The `Relaxed` load is sufficient here because the `Acquire`
            // exchange in the loop will synchronize memory.
            while self.lock.load(Ordering::Relaxed) {
                spin_loop();
            }
        }

        SpinLockIrqGuard {
            lock: self,
            irq_flags: saved_irq_flags,
            _marker: PhantomData,
        }
    }
}

impl<T, CPU: CpuOps> SpinLockIrq<T, CPU> {
    /// Unwrap an owned version of the spinlock into T, disregarding the
    /// spinlock.
    ///
    /// This is safe as if the compiler can guarantee exclusive access through
    /// the borrow checker then
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

/// An RAII guard for an IRQ-safe spinlock.
///
/// When this guard is dropped, the spinlock is released and the original
/// interrupt state of the local CPU core is restored.
#[must_use]
pub struct SpinLockIrqGuard<'a, T: ?Sized + 'a, CPU: CpuOps> {
    lock: &'a SpinLockIrq<T, CPU>,
    irq_flags: CPU::InterruptFlags,
    _marker: PhantomData<*const ()>, // !Send
}

/// A mapped RAII guard for an IRQ-safe spinlock.
///
/// An RAII mutex guard returned by `SpinLockIrqGuard::map`, which can point to
/// a subfield of the protected data. When this structure is dropped (falls out
/// of scope), the lock will be unlocked.
#[must_use]
pub struct SpinLockIrqMappedGuard<'a, T: ?Sized + 'a, U: ?Sized + 'a, CPU: CpuOps> {
    lock: &'a SpinLockIrq<T, CPU>,
    data: NonNull<U>,
    irq_flags: CPU::InterruptFlags,
    _marker: PhantomData<*const ()>, // !Send
}

impl<'a, T: ?Sized + 'a, CPU: CpuOps> SpinLockIrqGuard<'a, T, CPU> {
    /// Makes a new `SpinLockIrqMappedGuard` for a component of the locked data.
    ///
    /// The lock is not released and interrupts are not restored: ownership of
    /// both is transferred to the returned guard, which releases the lock and
    /// restores interrupts when it is dropped.
    ///
    /// This is an associated function that needs to be used as
    /// `SpinLockIrqGuard::map(...)` rather than a method on `self`, so as not
    /// to interfere with methods of the same name on the contents of the guard.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let guard = lock.lock_save_irq();
    /// let count = SpinLockIrqGuard::map(guard, |state| &mut state.count);
    /// // `count` derefs to the `u32` field, and still holds the lock.
    /// ```
    pub fn map<U, F>(orig: Self, f: F) -> SpinLockIrqMappedGuard<'a, T, U, CPU>
    where
        F: FnOnce(&mut T) -> &mut U,
        U: ?Sized,
    {
        // SAFETY: We hold the lock for the whole of `f`, and interrupts are disabled on
        // this core, so no other core and no interrupt handler on this core can hold a
        // reference to the data. We never dereference `orig` ourselves, so the `&mut T`
        // given to `f` is unique. On success `orig` is `ManuallyDrop`'d, so the lock
        // stays held and is released by the returned guard instead.
        let data = NonNull::from(f(unsafe { orig.lock.data.get().as_mut_unchecked() }));
        let orig = ManuallyDrop::new(orig);
        SpinLockIrqMappedGuard {
            data,
            lock: orig.lock,
            irq_flags: orig.irq_flags,
            _marker: PhantomData,
        }
    }

    /// Attempts to make a new `SpinLockIrqMappedGuard` for a component of the
    /// locked data.
    ///
    /// Returns `Err(orig)` with the original guard if `f` returns `None`, so
    /// the caller keeps the lock and can try a different projection without
    /// re-acquiring it. Dropping that returned guard releases the lock and
    /// restores interrupts as usual.
    ///
    /// On success the lock is not released and interrupts are not restored:
    /// ownership of both is transferred to the mapped guard.
    ///
    /// This is an associated function that needs to be used as
    /// `SpinLockIrqGuard::try_map(...)` rather than a method on `self`, so as
    /// not to interfere with methods of the same name on the contents of the
    /// guard.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let guard = lock.lock_save_irq();
    /// match SpinLockIrqGuard::try_map(guard, |collection| collection.get_mut(&id)) {
    ///     Ok(found) => { /* found; lock held by `found` */ }
    ///     Err(orig) => { /* absent; lock still held by `guard`, returned as `orig` */ }
    /// }
    /// ```
    pub fn try_map<U, F>(
        orig: Self,
        f: F,
    ) -> core::result::Result<SpinLockIrqMappedGuard<'a, T, U, CPU>, Self>
    where
        F: FnOnce(&mut T) -> Option<&mut U>,
        U: ?Sized,
    {
        // SAFETY: We hold the lock for the whole of `f`, and interrupts are disabled on
        // this core, so no other core and no interrupt handler on this core can hold a
        // reference to the data. We never dereference `orig` ourselves, so the `&mut T`
        // given to `f` is unique. On success `orig` is `ManuallyDrop`'d, so the lock
        // stays held and is released by the returned guard instead.
        let data = NonNull::from(
            match f(unsafe { orig.lock.data.get().as_mut_unchecked() }) {
                Some(x) => x,
                None => return Err(orig),
            },
        );

        let orig = ManuallyDrop::new(orig);

        Ok(SpinLockIrqMappedGuard {
            data,
            lock: orig.lock,
            irq_flags: orig.irq_flags,
            _marker: PhantomData,
        })
    }
}

impl<'a, T: ?Sized, CPU: CpuOps> Deref for SpinLockIrqGuard<'a, T, CPU> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The spinlock is held, guaranteeing exclusive access.
        // Interrupts are disabled on the local core, preventing re-entrant
        // access from an interrupt handler on this same core.
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T: ?Sized + 'a, U: ?Sized + 'a, CPU: CpuOps> Deref
    for SpinLockIrqMappedGuard<'a, T, U, CPU>
{
    type Target = U;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The spinlock is held, guaranteeing exclusive access.
        // Interrupts are disabled on the local core, preventing re-entrant
        // access from an interrupt handler on this same core.
        unsafe { self.data.as_ref() }
    }
}

impl<'a, T: ?Sized, CPU: CpuOps> DerefMut for SpinLockIrqGuard<'a, T, CPU> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: The spinlock is held, guaranteeing exclusive access.
        // Interrupts are disabled on the local core, preventing re-entrant
        // access from an interrupt handler on this same core.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<'a, T: ?Sized + 'a, U: ?Sized + 'a, CPU: CpuOps> DerefMut
    for SpinLockIrqMappedGuard<'a, T, U, CPU>
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: The spinlock is held, guaranteeing exclusive access.
        // Interrupts are disabled on the local core, preventing re-entrant
        // access from an interrupt handler on this same core.
        unsafe { self.data.as_mut() }
    }
}

impl<'a, T: ?Sized, CPU: CpuOps> Drop for SpinLockIrqGuard<'a, T, CPU> {
    /// Releases the lock and restores the previous interrupt state.
    fn drop(&mut self) {
        self.lock.lock.store(false, Ordering::Release);

        CPU::restore_interrupt_state(self.irq_flags);
    }
}

impl<'a, T: ?Sized + 'a, U: ?Sized + 'a, CPU: CpuOps> Drop
    for SpinLockIrqMappedGuard<'a, T, U, CPU>
{
    fn drop(&mut self) {
        self.lock.lock.store(false, Ordering::Release);

        CPU::restore_interrupt_state(self.irq_flags);
    }
}
