//! A module for creating lock-free, per-CPU static variables.
//!
//! This module provides a mechanism for creating data that is unique to each
//! processor core. Accessing this data is extremely fast as it requires no
//! locks.
//!
//! The design relies on a custom linker section (`.percpu`) and a macro
//! (`per_cpu!`) to automatically register and initialize all per-CPU variables
//! at boot.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::{Ref, RefCell, RefMut};
use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicPtr, Ordering};
use log::info;

use crate::CpuOps;

/// A wrapper for a RefCell guard (G) that restores interrupts on drop.
pub struct IrqGuard<G, CPU: CpuOps> {
    guard: ManuallyDrop<G>,
    flags: usize,
    _phantom: PhantomData<CPU>,
}

impl<G, CPU: CpuOps> IrqGuard<G, CPU> {
    fn new(guard: G, flags: usize) -> Self {
        Self {
            guard: ManuallyDrop::new(guard),
            flags,
            _phantom: PhantomData,
        }
    }
}

impl<G, CPU: CpuOps> Drop for IrqGuard<G, CPU> {
    fn drop(&mut self) {
        // Enaure we drop the refcell guard prior to restoring interrupts.
        unsafe { ManuallyDrop::drop(&mut self.guard) };

        CPU::restore_interrupt_state(self.flags);
    }
}

impl<G, CPU: CpuOps> Deref for IrqGuard<G, CPU>
where
    G: Deref,
{
    type Target = G::Target;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<G, CPU: CpuOps> DerefMut for IrqGuard<G, CPU>
where
    G: DerefMut,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

pub struct IrqSafeRefMut<'a, T, CPU: CpuOps> {
    borrow: ManuallyDrop<RefMut<'a, T>>,
    flags: usize,
    _phantom: PhantomData<CPU>,
}

impl<'a, T, CPU: CpuOps> core::ops::Deref for IrqSafeRefMut<'a, T, CPU> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.borrow
    }
}

impl<'a, T, CPU: CpuOps> core::ops::DerefMut for IrqSafeRefMut<'a, T, CPU> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.borrow
    }
}

impl<'a, T, CPU: CpuOps> Drop for IrqSafeRefMut<'a, T, CPU> {
    fn drop(&mut self) {
        // Ensure we release the refcell lock prior to enabling interrupts.
        unsafe {
            ManuallyDrop::drop(&mut self.borrow);
        }
        CPU::restore_interrupt_state(self.flags);
    }
}

/// A trait for type-erased initialization of `PerCpu` variables.
///
/// This allows the global initialization loop to call `init` on any `PerCpu<T>`
/// without knowing the concrete type `T`.
pub trait PerCpuInitializer {
    /// Initializes the per-CPU data allocation.
    fn init(&self, num_cpus: usize);
}

/// A container for a value that has a separate instance for each CPU core.
///
/// See the module-level documentation for detailed usage instructions.
pub struct PerCpu<T: Send, CPU: CpuOps> {
    /// A pointer to the heap-allocated array of `RefCell<T>`s, one for each
    /// CPU. It's `AtomicPtr` to ensure safe one-time initialization.
    ptr: AtomicPtr<RefCell<T>>,
    /// A function pointer to the initializer for type `T`.
    /// This is stored so it can be called during the runtime `init` phase.
    initializer: fn() -> T,

    phantom: PhantomData<CPU>,
}

impl<T: Send, CPU: CpuOps> PerCpu<T, CPU> {
    /// Creates a new, uninitialized `PerCpu` variable.
    ///
    // This is `const` so it can be used to initialize `static` variables.
    pub const fn new(initializer: fn() -> T) -> Self {
        Self {
            ptr: AtomicPtr::new(core::ptr::null_mut()),
            initializer,
            phantom: PhantomData,
        }
    }

    /// Returns a reference to the underlying datakj for the current CPU.
    ///
    /// # Panics Panics if the `PerCpu` variable has not been initialized.
    fn get_cell(&self) -> &RefCell<T> {
        let id = CPU::id();
        let base_ptr = self.ptr.load(Ordering::Acquire);

        if base_ptr.is_null() {
            panic!("PerCpu variable accessed before initialization");
        }

        // SAFETY: We have checked for null, and `init` guarantees the allocation
        // is valid for `id`. The returned reference is to a `RefCell`, which
        // manages its own internal safety.
        unsafe { &*base_ptr.add(id) }
    }

    /// Immutably borrows the per-CPU data.
    ///
    /// The borrow lasts until the returned `Ref<T>` is dropped.
    ///
    /// # Panics
    /// Panics if the value is already mutably borrowed.
    #[track_caller]
    pub fn borrow(&self) -> IrqGuard<Ref<'_, T>, CPU> {
        let flags = CPU::disable_interrupts();
        IrqGuard::new(self.get_cell().borrow(), flags)
    }

    /// Mutably borrows the per-CPU data.
    ///
    /// The borrow lasts until the returned `RefMut<T>` is dropped.
    ///
    /// # Panics
    /// Panics if the value is already borrowed (mutably or immutably).
    #[track_caller]
    pub fn borrow_mut(&self) -> IrqGuard<RefMut<'_, T>, CPU> {
        let flags = CPU::disable_interrupts();
        IrqGuard::new(self.get_cell().borrow_mut(), flags)
    }

    /// Attempts to immutably borrow the per-CPU data.
    #[track_caller]
    pub fn try_borrow(&self) -> Option<IrqGuard<Ref<'_, T>, CPU>> {
        let flags = CPU::disable_interrupts();

        match self.get_cell().try_borrow().ok() {
            Some(guard) => Some(IrqGuard::new(guard, flags)),
            None => {
                CPU::restore_interrupt_state(flags);
                None
            }
        }
    }

    #[track_caller]
    pub fn try_borrow_mut(&self) -> Option<IrqGuard<RefMut<'_, T>, CPU>> {
        let flags = CPU::disable_interrupts();

        match self.get_cell().try_borrow_mut().ok() {
            Some(guard) => Some(IrqGuard::new(guard, flags)),
            None => {
                CPU::restore_interrupt_state(flags);
                None
            }
        }
    }

    /// A convenience method to execute a closure with a mutable reference.
    /// This is often simpler than holding onto the `RefMut` guard.
    ///
    /// # Panics
    /// Panics if the value is already borrowed.
    pub fn with_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        f(&mut *self.borrow_mut())
    }
}

// Implement the type-erased initializer trait.
impl<T: Send, CPU: CpuOps> PerCpuInitializer for PerCpu<T, CPU> {
    fn init(&self, num_cpus: usize) {
        let mut values = Vec::with_capacity(num_cpus);
        for _ in 0..num_cpus {
            values.push(RefCell::new((self.initializer)()));
        }

        let leaked_ptr = Box::leak(values.into_boxed_slice()).as_mut_ptr();

        let result = self.ptr.compare_exchange(
            core::ptr::null_mut(),
            leaked_ptr,
            Ordering::Release,
            Ordering::Relaxed, // We don't care about the value on failure.
        );

        if result.is_err() {
            panic!("PerCpu::init called more than once on the same variable");
        }
    }
}

/// A `PerCpu<T>` can be safely shared between threads (`Sync`) if `T` is
/// `Send`.
///
/// # Safety
///
/// This is safe because although the `PerCpu` object itself is shared, the
/// underlying data `T` is partitioned. Each CPU can only access its own private
/// slot. The `T` value is effectively "sent" to its destination CPU's slot
/// during initialization. There is no cross-CPU data sharing at the `T` level.
unsafe impl<T: Send, CPU: CpuOps> Sync for PerCpu<T, CPU> {}

/// Initializes all `PerCpu` static variables defined with the `per_cpu!` macro.
///
/// This function iterates over the `.percpu` ELF section, which contains a list
/// of all `PerCpu` instances that need to be initialized.
///
/// # Safety
///
/// This function must only be called once during boot, before other cores are
/// started and before any `PerCpu` variable is accessed. It dereferences raw
/// pointers provided by the linker script. The caller must ensure that the
/// `__percpu_start` and `__percpu_end` symbols from the linker are valid.
pub unsafe fn setup_percpu(num_cpus: usize) {
    unsafe extern "C" {
        static __percpu_start: u8;
        static __percpu_end: u8;
    }

    let start_ptr =
        unsafe { &__percpu_start } as *const u8 as *const &'static (dyn PerCpuInitializer + Sync);
    let end_ptr =
        unsafe { &__percpu_end } as *const u8 as *const &'static (dyn PerCpuInitializer + Sync);

    let mut current_ptr = start_ptr;
    let mut objs_setup = 0;

    while current_ptr < end_ptr {
        let percpu_var = unsafe { &*current_ptr };
        percpu_var.init(num_cpus);
        current_ptr = unsafe { current_ptr.add(1) };
        objs_setup += 1;
    }

    info!("Setup {} per_cpu objects.", objs_setup * num_cpus);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::thread;

    thread_local! {
        static MOCK_CPU_ID: Cell<usize> = Cell::new(0);
    }

    struct MockArch;

    impl CpuOps for MockArch {
        fn id() -> usize {
            MOCK_CPU_ID.with(|id| id.get())
        }

        fn halt() -> ! {
            unimplemented!()
        }

        fn disable_interrupts() -> usize {
            0
        }

        fn restore_interrupt_state(_flags: usize) {}

        fn enable_interrupts() {}
    }

    #[test]
    fn test_initialization_and_basic_access() {
        let data: PerCpu<_, MockArch> = PerCpu::new(|| 0u32);
        data.init(4); // Simulate a 4-core system

        // Act as CPU 0
        MOCK_CPU_ID.with(|id| id.set(0));
        assert_eq!(*data.borrow(), 0);
        *data.borrow_mut() = 100;
        assert_eq!(*data.borrow(), 100);

        // Act as CPU 3
        MOCK_CPU_ID.with(|id| id.set(3));
        assert_eq!(*data.borrow(), 0); // Should still be the initial value
        data.with_mut(|val| *val = 300);
        assert_eq!(*data.borrow(), 300);

        // Check CPU 0 again to ensure it wasn't affected
        MOCK_CPU_ID.with(|id| id.set(0));
        assert_eq!(*data.borrow(), 100);
    }

    #[test]
    #[should_panic(expected = "PerCpu variable accessed before initialization")]
    fn test_panic_on_uninitialized_access() {
        let data: PerCpu<_, MockArch> = PerCpu::new(|| 0);
        // This should panic because data.init() was not called.
        let _ = data.borrow();
    }

    #[test]
    #[should_panic(expected = "PerCpu::init called more than once")]
    fn test_panic_on_double_init() {
        let data: PerCpu<_, MockArch> = PerCpu::new(|| 0);
        data.init(1);
        data.init(1); // This second call should panic.
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn test_refcell_panic_on_double_mutable_borrow() {
        let data: PerCpu<_, MockArch> = PerCpu::new(|| String::from("hello"));
        data.init(1);
        MOCK_CPU_ID.with(|id| id.set(0));

        let _guard1 = data.borrow_mut();
        // This second mutable borrow on the same "CPU" should panic.
        let _guard2 = data.borrow_mut();
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn test_refcell_panic_on_mutable_while_immutable_borrow() {
        let data: PerCpu<_, MockArch> = PerCpu::new(|| 0);
        data.init(1);
        MOCK_CPU_ID.with(|id| id.set(0));

        let _guard = data.borrow();
        // Attempting to mutably borrow while an immutable borrow exists should panic.
        *data.borrow_mut() = 5;
    }

    #[test]
    fn test_multithreaded_access_is_isolated() {
        // This is the stress test. It gives high confidence that the `unsafe impl Sync`
        // is correct because each thread will perform many isolated operations.
        const NUM_THREADS: usize = 8;
        const ITERATIONS_PER_THREAD: usize = 1000;

        // The data must be in an Arc to be shared across threads.
        let per_cpu_data: Arc<PerCpu<_, MockArch>> = Arc::new(PerCpu::new(|| 0));
        per_cpu_data.init(NUM_THREADS);

        let mut handles = vec![];

        for i in 0..NUM_THREADS {
            let data_clone = Arc::clone(&per_cpu_data);

            let handle = thread::spawn(move || {
                MOCK_CPU_ID.with(|id| id.set(i));

                let initial_val = i * 100_000;
                data_clone.with_mut(|val| *val = initial_val);

                for j in 0..ITERATIONS_PER_THREAD {
                    data_clone.with_mut(|val| {
                        // VERIFY: Check that the value is what we expect from
                        // the previous iteration. If another thread interfered,
                        // this assert will fail.
                        let expected_val = initial_val + j;
                        assert_eq!(
                            *val, expected_val,
                            "Data corruption on CPU {}! Expected {}, found {}",
                            i, expected_val, *val
                        );

                        // MODIFY: Increment the value.
                        *val += 1;
                    });

                    if j % 10 == 0 {
                        // Don't yield on every single iteration
                        thread::yield_now();
                    }
                }

                // After the loop, verify the final value.
                let final_val = *data_clone.borrow();
                let expected_final_val = initial_val + ITERATIONS_PER_THREAD;
                assert_eq!(
                    final_val, expected_final_val,
                    "Incorrect final value on CPU {}",
                    i
                );
            });
            handles.push(handle);
        }

        // Wait for all threads to complete.
        for handle in handles {
            handle.join().unwrap();
        }

        // Optional: Final sanity check from the main thread (acting as CPU 0)
        // to ensure its value was not corrupted by the other threads.
        MOCK_CPU_ID.with(|id| id.set(0));
        let expected_val_for_cpu0 = 0 * 100_000 + ITERATIONS_PER_THREAD;
        assert_eq!(*per_cpu_data.borrow(), expected_val_for_cpu0);
    }
}
