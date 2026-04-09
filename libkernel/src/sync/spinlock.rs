//! Low-level spin lock primitives.

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
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

/// An RAII guard for an IRQ-safe spinlock.
///
/// When this guard is dropped, the spinlock is released and the original
/// interrupt state of the local CPU core is restored.
#[must_use]
pub struct SpinLockIrqGuard<'a, T: ?Sized + 'a, CPU: CpuOps> {
    lock: &'a SpinLockIrq<T, CPU>,
    irq_flags: usize,                // The saved DAIF register state
    _marker: PhantomData<*const ()>, // !Send
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

impl<'a, T: ?Sized, CPU: CpuOps> DerefMut for SpinLockIrqGuard<'a, T, CPU> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: The spinlock is held, guaranteeing exclusive access.
        // Interrupts are disabled on the local core, preventing re-entrant
        // access from an interrupt handler on this same core.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<'a, T: ?Sized, CPU: CpuOps> Drop for SpinLockIrqGuard<'a, T, CPU> {
    /// Releases the lock and restores the previous interrupt state.
    fn drop(&mut self) {
        self.lock.lock.store(false, Ordering::Release);

        CPU::restore_interrupt_state(self.irq_flags);
    }
}
