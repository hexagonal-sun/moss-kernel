#![cfg_attr(not(test), no_std)]

#[cfg(feature = "paging")]
pub mod arch;
pub mod error;
#[cfg(feature = "fs")]
pub mod driver;
#[cfg(feature = "fs")]
pub mod fs;
pub mod memory;
#[cfg(feature = "fs")]
pub mod pod;
#[cfg(feature = "proc")]
pub mod proc;
#[cfg(feature = "sync")]
pub mod sync;

extern crate alloc;

#[cfg(feature = "paging")]
pub use memory::address_space::{
    KernAddressSpace, PageInfo, UserAddressSpace, VirtualMemory,
};

pub trait CpuOps: 'static {
    /// Returns the ID of the currently executing core.
    fn id() -> usize;

    /// Halts the CPU indefinitely.
    fn halt() -> !;

    /// Disables all maskable interrupts on the current CPU core, returning the
    /// previous state prior to masking.
    fn disable_interrupts() -> usize;

    /// Restore the previous interrupt state obtained from `disable_interrupts`.
    fn restore_interrupt_state(flags: usize);

    /// Explicitly enables maskable interrupts on the current CPU core.
    fn enable_interrupts();
}

#[cfg(test)]
pub mod test {
    use core::hint::spin_loop;

    use crate::CpuOps;

    // A CPU mock object that can be used in unit-tests.
    pub struct MockCpuOps {}

    impl CpuOps for MockCpuOps {
        fn id() -> usize {
            0
        }

        fn halt() -> ! {
            loop {
                spin_loop();
            }
        }

        fn disable_interrupts() -> usize {
            0
        }

        fn restore_interrupt_state(_flags: usize) {}

        fn enable_interrupts() {}
    }
}
