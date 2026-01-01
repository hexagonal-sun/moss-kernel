//! Manages the virtual memory address space of a process.

use crate::{
    UserAddressSpace,
    error::{KernelError, Result},
};
use memory_map::{AddressRequest, MemoryMap};
use vmarea::{AccessKind, FaultValidation, VMAPermissions, VMArea, VMAreaKind};

use super::{PAGE_SIZE, address::VA, region::VirtMemoryRegion};

pub mod memory_map;
pub mod vmarea;

const BRK_PERMISSIONS: VMAPermissions = VMAPermissions::rw();

pub struct ProcessVM<AS: UserAddressSpace> {
    mm: MemoryMap<AS>,
    brk: VirtMemoryRegion,
}

impl<AS: UserAddressSpace> ProcessVM<AS> {
    /// Constructs a new Process VM structure from the given VMA. The heap is
    /// placed *after* the given VMA.
    ///
    /// # Safety
    /// Any pages that have been mapped into the provided address space *must*
    /// corresponde to the provided VMA.
    pub unsafe fn from_vma_and_address_space(vma: VMArea, addr_spc: AS) -> Self {
        let mut mm = MemoryMap::with_addr_spc(addr_spc);

        mm.insert_and_merge(vma.clone());

        let brk = VirtMemoryRegion::new(vma.region.end_address().align_up(PAGE_SIZE), 0);

        Self { mm, brk }
    }

    /// Constructs a new Process VM structure from the given VMA. The heap is
    /// placed *after* the given VMA.
    pub fn from_vma(vma: VMArea) -> Result<Self> {
        let mut mm = MemoryMap::new()?;

        mm.insert_and_merge(vma.clone());

        let brk = VirtMemoryRegion::new(vma.region.end_address().align_up(PAGE_SIZE), 0);

        Ok(Self { mm, brk })
    }

    pub fn from_map(map: MemoryMap<AS>) -> Self {
        // Last entry will be the VMA with the highest address.
        let brk = map
            .vmas
            .last_key_value()
            .expect("No VMAs in map")
            .1
            .region
            .end_address()
            // VMAs should already be page-aligned, but just in case.
            .align_up(PAGE_SIZE);

        Self {
            mm: map,
            brk: VirtMemoryRegion::new(brk, 0),
        }
    }

    pub fn empty() -> Result<Self> {
        Ok(Self {
            mm: MemoryMap::new()?,
            brk: VirtMemoryRegion::empty(),
        })
    }

    pub fn find_vma_for_fault(&self, addr: VA, access_type: AccessKind) -> Option<&VMArea> {
        let vma = self.mm.find_vma(addr)?;

        match vma.validate_fault(addr, access_type) {
            FaultValidation::Valid => Some(vma),
            FaultValidation::NotPresent => unreachable!(""),
            FaultValidation::PermissionDenied => None,
        }
    }

    pub fn mm_mut(&mut self) -> &mut MemoryMap<AS> {
        &mut self.mm
    }

    pub fn current_brk(&self) -> VA {
        self.brk.end_address()
    }

    /// Resizes the program break (the heap).
    ///
    /// This function implements the semantics of the `brk` system call. It can
    /// either grow or shrink the heap area.
    ///
    /// # Arguments
    /// * `new_end_addr`: The desired new end address for the program break.
    ///
    /// # Returns
    /// * `Ok(())` on success.
    /// * `Err(KernelError)` on failure. This can happen if the requested memory
    ///   region conflicts with an existing mapping, or if the request is invalid
    ///   (e.g., shrinking the break below its initial start address).
    pub fn resize_brk(&mut self, new_end_addr: VA) -> Result<VA> {
        let brk_start = self.brk.start_address();
        let current_end = self.brk.end_address();

        // The break cannot be shrunk to an address lower than its starting
        // point.
        if new_end_addr < brk_start {
            return Err(KernelError::InvalidValue);
        }

        let new_end_addr_aligned = new_end_addr.align_up(PAGE_SIZE);

        let new_brk_region =
            VirtMemoryRegion::from_start_end_address(brk_start, new_end_addr_aligned);

        if new_end_addr_aligned == current_end {
            // The requested break is the same as the current one, or it is
            // within the same page as the existing allocation. This is a no-op.
            return Ok(new_end_addr);
        }

        // Grow the break
        if new_end_addr_aligned > current_end {
            let growth_size = new_end_addr_aligned.value() - current_end.value();

            self.mm.mmap(
                AddressRequest::Fixed {
                    address: current_end,
                    permit_overlap: false,
                },
                growth_size,
                BRK_PERMISSIONS,
                VMAreaKind::Anon,
            )?;

            self.brk = new_brk_region;

            return Ok(new_end_addr);
        }

        // Shrink the break
        // At this point, we know `new_end_aligned < current_end`.
        let unmap_region =
            VirtMemoryRegion::from_start_end_address(new_end_addr_aligned, current_end);
        self.mm.munmap(unmap_region)?;

        self.brk = new_brk_region;

        Ok(new_end_addr)
    }

    pub fn clone_as_cow(&mut self) -> Result<Self> {
        Ok(Self {
            mm: self.mm.clone_as_cow()?,
            brk: self.brk,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::memory_map::tests::MockAddressSpace;
    use super::*;
    use crate::error::KernelError;

    fn setup_vm() -> ProcessVM<MockAddressSpace> {
        let text_vma = VMArea {
            region: VirtMemoryRegion::new(VA::from_value(0x1000), PAGE_SIZE),
            kind: VMAreaKind::Anon, // Simplification for test
            permissions: VMAPermissions::rx(),
        };

        ProcessVM::from_vma(text_vma).unwrap()
    }

    #[test]
    fn test_initial_state() {
        // Given: a newly created ProcessVM
        let vm = setup_vm();

        let initial_brk_start = VA::from_value(0x1000 + PAGE_SIZE);
        assert_eq!(vm.brk.start_address(), initial_brk_start);
        assert_eq!(vm.brk.size(), 0);
        assert_eq!(vm.current_brk(), initial_brk_start);

        // And the break region itself should not be mapped
        assert!(vm.mm.find_vma(initial_brk_start).is_none());
    }

    #[test]
    fn test_brk_first_growth() {
        // Given: a VM with a zero-sized heap
        let mut vm = setup_vm();
        let initial_brk_start = vm.brk.start_address();
        let brk_addr = initial_brk_start.add_bytes(1);

        let new_brk = vm.resize_brk(brk_addr).unwrap();

        // The new break should be page-aligned
        let expected_brk_end = brk_addr.align_up(PAGE_SIZE);
        assert_eq!(new_brk, brk_addr);
        assert_eq!(vm.current_brk(), expected_brk_end);
        assert_eq!(vm.brk.size(), PAGE_SIZE);

        // And a new VMA for the heap should now exist with RW permissions
        let heap_vma = vm
            .mm
            .find_vma(initial_brk_start)
            .expect("Heap VMA should exist");
        assert_eq!(heap_vma.region.start_address(), initial_brk_start);
        assert_eq!(heap_vma.region.end_address(), expected_brk_end);
        assert_eq!(heap_vma.permissions, VMAPermissions::rw());
    }

    #[test]
    fn test_brk_subsequent_growth() {
        // Given: a VM with an existing heap
        let mut vm = setup_vm();
        let initial_brk_start = vm.brk.start_address();
        vm.resize_brk(initial_brk_start.add_bytes(1)).unwrap(); // First growth
        assert_eq!(vm.brk.size(), PAGE_SIZE);

        // When: we grow the break again
        let new_brk = vm.resize_brk(vm.current_brk().add_pages(1)).unwrap();

        // Then: the break should be extended
        let expected_brk_end = initial_brk_start.add_pages(2);
        assert_eq!(new_brk, expected_brk_end);
        assert_eq!(vm.current_brk(), expected_brk_end);
        assert_eq!(vm.brk.size(), 2 * PAGE_SIZE);

        // And the single heap VMA should be larger, not a new one
        let heap_vma = vm.mm.find_vma(initial_brk_start).unwrap();
        assert_eq!(heap_vma.region.end_address(), expected_brk_end);
        assert_eq!(vm.mm.vma_count(), 2); // Text VMA + one Heap VMA
    }

    #[test]
    fn test_brk_shrink() {
        // Given: a VM with a 3-page heap
        let mut vm = setup_vm();
        let initial_brk_start = vm.brk.start_address();
        vm.resize_brk(initial_brk_start.add_pages(3)).unwrap();
        assert_eq!(vm.brk.size(), 3 * PAGE_SIZE);

        // When: we shrink the break by one page
        let new_brk_addr = initial_brk_start.add_pages(2);
        let new_brk = vm.resize_brk(new_brk_addr).unwrap();

        // Then: the break should be updated
        assert_eq!(new_brk, new_brk_addr);
        assert_eq!(vm.current_brk(), new_brk_addr);
        assert_eq!(vm.brk.size(), 2 * PAGE_SIZE);

        // And the memory for the shrunken page should now be unmapped
        assert!(vm.mm.find_vma(new_brk_addr.add_bytes(1)).is_none());
        // But the remaining heap should still be mapped
        assert!(vm.mm.find_vma(initial_brk_start).is_some());
    }

    #[test]
    fn test_brk_shrink_to_zero() {
        // Given: a VM with a 2-page heap
        let mut vm = setup_vm();
        let initial_brk_start = vm.brk.start_address();
        vm.resize_brk(initial_brk_start.add_pages(2)).unwrap();

        // When: we shrink the break all the way back to its start
        let new_brk = vm.resize_brk(initial_brk_start).unwrap();

        // Then: the break should be zero-sized again
        assert_eq!(new_brk, initial_brk_start);
        assert_eq!(vm.current_brk(), initial_brk_start);
        assert_eq!(vm.brk.size(), 0);

        // And the heap VMA should be completely gone
        assert!(vm.mm.find_vma(initial_brk_start).is_none());
        assert_eq!(vm.mm.vma_count(), 1); // Only the text VMA remains
    }

    #[test]
    fn test_brk_no_op() {
        // Given: a VM with a 2-page heap
        let mut vm = setup_vm();
        let initial_brk_start = vm.brk.start_address();
        let current_brk_end = vm.resize_brk(initial_brk_start.add_pages(2)).unwrap();

        // When: we resize the break to its current end
        let new_brk = vm.resize_brk(current_brk_end).unwrap();

        // Then: nothing should change
        assert_eq!(new_brk, current_brk_end);
        assert_eq!(vm.brk.size(), 2 * PAGE_SIZE);
        assert_eq!(vm.mm.vma_count(), 2);
    }

    #[test]
    fn test_brk_invalid_shrink_below_start() {
        let mut vm = setup_vm();
        let initial_brk_start = vm.brk.start_address();
        vm.resize_brk(initial_brk_start.add_pages(1)).unwrap();
        let original_len = vm.brk.size();

        // We try to shrink the break below its starting point
        let result = vm.resize_brk(VA::from_value(initial_brk_start.value() - 1));

        // It should fail with an InvalidValue error
        assert!(matches!(result, Err(KernelError::InvalidValue)));

        // And the state of the break should not have changed
        assert_eq!(vm.brk.start_address(), initial_brk_start);
        assert_eq!(vm.brk.size(), original_len);
    }

    #[test]
    fn test_brk_growth_collision() {
        // Given: a VM with another mapping right where the heap would grow
        let mut vm = setup_vm();
        let initial_brk_start = vm.brk.start_address();
        let obstacle_addr = initial_brk_start.add_pages(2);

        let obstacle_vma = VMArea {
            region: VirtMemoryRegion::new(obstacle_addr, PAGE_SIZE),
            kind: VMAreaKind::Anon,
            permissions: VMAPermissions::ro(),
        };
        vm.mm.insert_and_merge(obstacle_vma);
        assert_eq!(vm.mm.vma_count(), 2);

        // When: we try to grow the break past the obstacle
        let result = vm.resize_brk(initial_brk_start.add_pages(3));

        // Then: the mmap should fail, resulting in an error
        // The specific error comes from your mmap implementation.
        assert!(matches!(result, Err(KernelError::InvalidValue)));

        // And the break should not have grown at all
        assert_eq!(vm.brk.size(), 0);
        assert_eq!(vm.current_brk(), initial_brk_start);
    }
}
