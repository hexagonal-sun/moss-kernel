//! # libkernel
//!
//! Architecture-independent kernel building blocks for operating systems.
//!
//!
//! `libkernel` provides the core abstractions that a kernel needs to manage
//! memory, processes, filesystems, and synchronisation — all without tying the
//! implementation to a particular CPU architecture. It is designed to run in a
//! `no_std` environment and relies on feature gates to keep the dependency
//! footprint minimal.
//!
//! ## Feature gates
//!
//! Most of the crate is hidden behind Cargo features so that consumers only pay
//! for the subsystems they need:
//!
//! | Feature   | Enables                                               | Implies          |
//! |-----------|-------------------------------------------------------|------------------|
//! | `sync`    | Synchronisation primitives (spinlock, mutex, rwlock…) | —                |
//! | `alloc`   | Memory allocators (buddy, slab) and collection types  | `sync`           |
//! | `paging`  | Page tables, PTE helpers                              | `alloc`          |
//! | `proc`    | Process identity types (UID/GID, capabilities)        | —                |
//! | `fs`      | VFS traits, path manipulation, block I/O              | `proc`, `sync`   |
//! | `proc_vm` | Process virtual-memory management (mmap, brk, CoW)    | `paging`, `fs`   |
//! | `kbuf`    | Async-aware circular kernel buffers                   | `sync`           |
//! | `all`     | Everything above                                      | all of the above |
//!
//! ## The `CpuOps` trait
//!
//! Nearly every synchronisation and memory primitive in the crate is generic
//! over a [`CpuOps`] implementation. This trait abstracts the handful of
//! arch-specific operations (core ID, interrupt masking, halt) that the
//! arch-independent code depends on, making the library portable while still
//! fully testable on the host.
//!
//! ## Crate layout
//!
//! - [`error`]  — Unified kernel error types and POSIX errno mapping.
//! - [`memory`] — Typed addresses, memory regions, page allocators, and
//!   address-space management.
//! - [`sync`]   — Async-aware spinlocks, mutexes, rwlocks, condvars, channels,
//!   and per-CPU storage *(feature `sync`)*.
//! - [`fs`]     — VFS traits (`Filesystem`, `Inode`, `BlockDevice`), path
//!   manipulation, and filesystem driver scaffolding *(feature `fs`)*.
//! - [`proc`]   — Process identity types and Linux-compatible capabilities
//!   *(feature `proc`)*.
//! - [`arch`]   — Architecture-specific support code *(feature `paging`)*.

#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

#[cfg(feature = "paging")]
pub mod arch;
#[cfg(feature = "fs")]
pub mod driver;
pub mod error;
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

/// Trait abstracting the small set of CPU operations that the
/// architecture-independent kernel code requires.
///
/// Every concrete kernel target must provide an implementation of this trait.
/// The synchronisation primitives in [`sync`] and the memory subsystem in
/// [`memory`] are generic over `CpuOps`, which keeps this crate portable while
/// allowing the real kernel — and unit tests — to supply their own
/// implementations.
///
/// # Example (test mock)
///
/// ```
/// use libkernel::CpuOps;
///
/// struct MockCpu;
///
/// impl CpuOps for MockCpu {
///     type InterruptFlags = usize;
///     fn id() -> usize { 0 }
///     fn halt() -> ! { loop { core::hint::spin_loop() } }
///     fn disable_interrupts() -> usize { 0 }
///     fn restore_interrupt_state(_flags: usize) {}
///     fn enable_interrupts() {}
/// }
/// ```
pub trait CpuOps: 'static {
    /// The type of the register that contains the interrupt flag enable state.
    type InterruptFlags: Clone + Copy;

    /// Returns the ID of the currently executing core.
    fn id() -> usize;

    /// Halts the CPU indefinitely.
    fn halt() -> !;

    /// Disables all maskable interrupts on the current CPU core, returning the
    /// previous state prior to masking.
    fn disable_interrupts() -> Self::InterruptFlags;

    /// Restore the previous interrupt state obtained from `disable_interrupts`.
    fn restore_interrupt_state(flags: Self::InterruptFlags);

    /// Explicitly enables maskable interrupts on the current CPU core.
    fn enable_interrupts();
}

/// Generic stack trace methods
pub trait StackTrace: Sized {
    /// Start a stack trace, if supported.
    ///
    /// # Safety
    /// Will mess up stack
    unsafe fn start() -> Option<Self> {
        None
    }

    /// Continue to next frame in stack, returning `None` if there are no more frames or if the next frame is invalid.
    ///
    /// # Safety
    /// Will mess up stack
    unsafe fn next(self) -> Option<Self> {
        None
    }

    /// Frame pointer
    fn fp(&self) -> usize;
    /// PC PTR
    fn pc_ptr(&self) -> *const usize;
}

#[cfg(test)]
#[allow(missing_docs)]
pub mod test {
    use core::hint::spin_loop;

    use crate::CpuOps;

    // A CPU mock object that can be used in unit-tests.
    pub struct MockCpuOps {}

    impl CpuOps for MockCpuOps {
        type InterruptFlags = usize;

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
