use crate::{Result};
use crate::memory::{page::PageFrame, permissions::PtePermissions, region::VirtMemoryRegion};
use crate::{UserAddressSpace, KernAddressSpace};
use crate::memory::address::VA;
use crate::error::KernelError;

use core::arch::asm;
use alloc::vec::Vec;


pub struct X86_64ProcessAddressSpace {
    pub cr3: u64,
}

impl X86_64ProcessAddressSpace {
    // Helper to write CR3
    unsafe fn write_cr3(cr3: u64) {
        // TODO: Implement CR3 write for x86_64 when the target toolchain
        // and inline-asm constraints are confirmed. For now, keep this
        // as a no-op so the crate builds across targets.
        let _ = cr3;
    }
}

impl UserAddressSpace for X86_64ProcessAddressSpace {
    fn new() -> Result<Self> where Self: Sized {
        // TODO: Allocate a top-level page table (PML4) and possibly copy kernel mappings.
        // For now, we stub this or just allocate a dummy page if allocator works.
        // Assuming we need a physical page for PML4.
        
        // This is a placeholder. Real implementation needs a physical frame allocator.
        // But since we are just unblocking compilation and basic logic:
        Ok(Self { cr3: 0 }) 
    }

    fn activate(&self) {
        if self.cr3 != 0 {
            unsafe { Self::write_cr3(self.cr3) };
        }
    }

    fn deactivate(&self) {
        // No-op generally on x86, kernel mappings persist.
    }

    fn map_page(&mut self, _page: PageFrame, _va: VA, _perms: PtePermissions) -> Result<()> {
        // TODO: Walking page tables
        Ok(())
    }

    fn unmap(&mut self, _va: VA) -> Result<PageFrame> {
        Err(KernelError::NotSupported)
    }

    fn remap(&mut self, _va: VA, _new_page: PageFrame, _perms: PtePermissions) -> Result<PageFrame> {
        Err(KernelError::NotSupported)
    }

    fn protect_range(&mut self, _va_range: VirtMemoryRegion, _perms: PtePermissions) -> Result<()> {
        Err(KernelError::NotSupported)
    }

    fn unmap_range(&mut self, _va_range: VirtMemoryRegion) -> Result<Vec<PageFrame>> {
        Ok(Vec::new())
    }

    fn translate(&self, _va: VA) -> Option<crate::PageInfo> {
        None
    }

    fn protect_and_clone_region(&mut self, _region: VirtMemoryRegion, _other: &mut Self, _perms: PtePermissions) -> Result<()> where Self: Sized {
        Ok(())
    }
}

pub struct X86_64KernelAddressSpace {}

impl KernAddressSpace for X86_64KernelAddressSpace {
    fn map_mmio(&mut self, _region: crate::memory::region::PhysMemoryRegion) -> Result<VA> {
        Err(KernelError::NotSupported)
    }

    fn map_normal(&mut self, _phys_range: crate::memory::region::PhysMemoryRegion, _virt_range: VirtMemoryRegion, _perms: PtePermissions) -> Result<()> {
        Ok(())
    }
}
