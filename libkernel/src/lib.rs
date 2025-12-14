#![cfg_attr(not(test), no_std)]

use alloc::vec::Vec;
use error::Result;
use memory::{
    address::VA,
    page::PageFrame,
    permissions::PtePermissions,
    region::{PhysMemoryRegion, VirtMemoryRegion},
};
use sync::spinlock::SpinLockIrq;

pub mod arch;
pub mod driver;
pub mod error;
pub mod fs;
pub mod memory;
pub mod pod;
pub mod proc;
pub mod sync;

extern crate alloc;

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

/// An architecture-independent representation of a page table entry (PTE).
pub struct PageInfo {
    pub pfn: PageFrame,
    pub perms: PtePermissions,
}

/// Represents a process's memory context, abstracting the hardware-specific
/// details of page tables, address translation, and TLB management.
///
/// This trait defines the fundamental interface that the kernel's
/// architecture-independent memory management code uses to interact with an
/// address space. Each supported architecture must provide a concrete
/// implementation.
pub trait UserAddressSpace: Send + Sync {
    /// Creates a new, empty page table hierarchy for a new process.
    ///
    /// The resulting address space should be configured for user space access
    /// but will initially contain no user mappings. It is the responsibility of
    /// the implementation to also map the kernel's own address space into the
    /// upper region of the virtual address space, making kernel code and data
    /// accessible.
    ///
    /// # Returns
    ///
    /// `Ok(Self)` on success, or an `Err` if memory for the top-level page
    /// table could not be allocated.
    fn new() -> Result<Self>
    where
        Self: Sized;

    /// Activates this address space for the current CPU.
    ///
    /// The implementation must load the address of this space's root page table
    /// into the appropriate CPU register (e.g., `CR3` on x86-64 or `TTBR0_EL1`
    /// on AArch64). This action makes the virtual address mappings defined by
    /// this space active on the current core.
    ///
    /// The implementation must also ensure that any necessary TLB invalidations
    /// occur so that stale translations from the previously active address
    /// space are flushed.
    fn activate(&self);

    /// Decativate this address space for the current CPU.
    ///
    /// This should be called to leave the CPU without any current process
    /// state. Used on process termination code-paths.
    fn deactivate(&self);

    /// Maps a single physical page frame to a virtual address.
    ///
    /// This function creates a page table entry (PTE) that maps the given
    /// physical `page` to the specified virtual address `va` with the provided
    /// `perms`. The implementation must handle the allocation and setup of any
    /// intermediate page tables (e.g., L1 or L2 tables) if they do not already
    /// exist.
    ///
    /// # Arguments
    ///
    /// * `page`: The `PageFrame` of physical memory to map.
    /// * `va`: The page-aligned virtual address to map to.
    /// * `perms`: The `PtePermissions` to apply to the mapping.
    ///
    /// # Errors
    ///
    /// Returns an error if a mapping already exists at `va` or if memory for
    /// intermediate page tables cannot be allocated.
    fn map_page(&mut self, page: PageFrame, va: VA, perms: PtePermissions) -> Result<()>;

    /// Unmaps a single virtual page, returning the physical page it was mapped
    /// to.
    ///
    /// The implementation must invalidate the PTE for the given `va` and ensure
    /// the corresponding TLB entry is flushed.
    ///
    /// # Returns
    ///
    /// The `PageFrame` that was previously mapped at `va`. This allows the
    /// caller to manage the lifecycle of the physical memory (e.g., decrement a
    /// reference count or free it). Returns an error if no page is mapped at
    /// `va`.
    fn unmap(&mut self, va: VA) -> Result<PageFrame>;

    /// Atomically unmaps a page at `va` and maps a new page in its place.
    ///
    /// # Returns
    ///
    /// The `PageFrame` of the *previously* mapped page, allowing the caller to
    /// manage its lifecycle. Returns an error if no page was originally mapped
    /// at `va`.
    fn remap(&mut self, va: VA, new_page: PageFrame, perms: PtePermissions) -> Result<PageFrame>;

    /// Changes the protection flags for a range of virtual addresses.
    ///
    /// This is the low-level implementation for services like `mprotect`. It
    /// walks the page tables for the given `va_range` and updates the
    /// permissions of each PTE to match `perms`.
    ///
    /// The implementation must ensure that the TLB is invalidated for the
    /// entire range.
    fn protect_range(&mut self, va_range: VirtMemoryRegion, perms: PtePermissions) -> Result<()>;

    /// Unmaps an entire range of virtual addresses.
    ///
    /// This is the low-level implementation for services like `munmap`. It
    /// walks the page tables for the given `va_range` and invalidates all PTEs
    /// within it.
    ///
    /// # Returns
    ///
    /// A `Vec<PageFrame>` containing all the physical frames that were
    /// unmapped. This allows the caller to free all associated physical memory.
    fn unmap_range(&mut self, va_range: VirtMemoryRegion) -> Result<Vec<PageFrame>>;

    /// Translates a virtual address to its corresponding physical mapping
    /// information.
    ///
    /// This function performs a page table walk to find the PTE for the given
    /// `va`. It is a read-only operation used by fault handlers and other
    /// kernel subsystems to inspect the state of a mapping.
    ///
    /// # Returns
    ///
    /// `Some(PageInfo)` containing the mapped physical frame and the *actual*
    /// hardware and software permissions from the PTE, or `None` if no valid
    /// mapping exists for `va`.
    fn translate(&self, va: VA) -> Option<PageInfo>;

    /// Atomically protects a region in the source address space and clones the
    /// mappings into a destination address space.
    ///
    /// This is the core operation for implementing an efficient `fork()` system
    /// call using Copy-on-Write (CoW). It is specifically optimized to perform
    /// the protection of the parent's pages and the creation of the child's
    /// shared mappings in a single, atomic walk of the source page tables.
    ///
    /// ## Operation
    ///
    /// For each present Page Table Entry (PTE) within the `region` of `self`:
    ///
    /// 1. The physical frame's reference count is incremented to reflect the
    ///    new mapping in `other`. This *must* be done by the
    ///    architecture-specific implementation.
    /// 2. The PTE in the `other` (child) address space is created, mapping the
    ///    same physical frame at the same virtual address with the given
    ///    `perms`.
    /// 3. The PTE in the `self` (parent) address space has its permissions
    ///    updated to `perms`.
    ///
    /// # Arguments
    ///
    /// * `&mut self`: The source (parent) address space. Its permissions for
    ///   the region will be modified.
    /// * `region`: The virtual memory region to operate on.
    /// * `other`: The destination (child) address space.
    /// * `perms`: The `PtePermissions` to set for the mappings in both `self`
    ///   and `other`.
    fn protect_and_clone_region(
        &mut self,
        region: VirtMemoryRegion,
        other: &mut Self,
        perms: PtePermissions,
    ) -> Result<()>
    where
        Self: Sized;
}

/// Represents the kernel's memory context.
pub trait KernAddressSpace: Send {
    /// Map the given region as MMIO memory.
    fn map_mmio(&mut self, region: PhysMemoryRegion) -> Result<VA>;

    /// Map the given region as normal memory.
    fn map_normal(
        &mut self,
        phys_range: PhysMemoryRegion,
        virt_range: VirtMemoryRegion,
        perms: PtePermissions,
    ) -> Result<()>;
}

/// The types and functions required for the virtual memory subsystem.
pub trait VirtualMemory: CpuOps + Sized {
    /// The type representing an entry in the top-level page table. For AArch64
    /// this is L0, for x86_64 it's PML4.
    type PageTableRoot;

    /// The address space type used for all user-space processes.
    type ProcessAddressSpace: UserAddressSpace;

    /// The address space type used for the kernel.
    type KernelAddressSpace: KernAddressSpace;

    /// The starting address for the logical mapping of all physical ram.
    const PAGE_OFFSET: usize;

    /// Obtain a reference to the kernel's address space.
    fn kern_address_space() -> &'static SpinLockIrq<Self::KernelAddressSpace, Self>;
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
