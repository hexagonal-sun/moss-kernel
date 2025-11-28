use super::MemoryMap;
use crate::{
    PageInfo, UserAddressSpace,
    error::Result,
    fs::Inode,
    memory::{
        PAGE_SIZE,
        address::VA,
        page::PageFrame,
        permissions::PtePermissions,
        proc_vm::{
            memory_map::{AddressRequest, MMAP_BASE},
            vmarea::{VMAPermissions, VMArea, VMAreaKind, VMFileMapping, tests::DummyTestInode},
        },
        region::VirtMemoryRegion,
    },
};
use alloc::sync::Arc;
use std::sync::Mutex;

/// Represents a single operation performed on the mock page table.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum MockPageTableOp {
    UnmapRange {
        region: VirtMemoryRegion,
    },
    ProtectRange {
        region: VirtMemoryRegion,
        perms: PtePermissions,
    },
}

pub struct MockAddressSpace {
    pub ops_log: Mutex<Vec<MockPageTableOp>>,
}

impl UserAddressSpace for MockAddressSpace {
    fn new() -> Result<Self> {
        Ok(Self {
            ops_log: Mutex::new(Vec::new()),
        })
    }

    fn activate(&self) {
        unimplemented!()
    }
    fn deactivate(&self) {
        unimplemented!()
    }

    fn map_page(&mut self, _page: PageFrame, _va: VA, _perms: PtePermissions) -> Result<()> {
        panic!("Should be called by the demand-pager");
    }

    fn unmap(&mut self, va: VA) -> Result<PageFrame> {
        let region = VirtMemoryRegion::new(va, PAGE_SIZE);
        self.ops_log
            .lock()
            .unwrap()
            .push(MockPageTableOp::UnmapRange { region });
        // Return a dummy page, as the caller doesn't use it.
        Ok(PageFrame::from_pfn(0))
    }

    fn protect_range(&mut self, va_range: VirtMemoryRegion, perms: PtePermissions) -> Result<()> {
        self.ops_log
            .lock()
            .unwrap()
            .push(MockPageTableOp::ProtectRange {
                region: va_range,
                perms,
            });
        Ok(())
    }

    fn unmap_range(&mut self, va_range: VirtMemoryRegion) -> Result<Vec<PageFrame>> {
        self.ops_log
            .lock()
            .unwrap()
            .push(MockPageTableOp::UnmapRange { region: va_range });
        Ok(Vec::new())
    }

    fn translate(&self, _va: VA) -> Option<PageInfo> {
        None
    }

    fn protect_and_clone_region(
        &mut self,
        _region: VirtMemoryRegion,
        _other: &mut Self,
        _perms: PtePermissions,
    ) -> Result<()>
    where
        Self: Sized,
    {
        unreachable!("Not called")
    }

    fn remap(
        &mut self,
        _va: VA,
        _new_page: PageFrame,
        _perms: PtePermissions,
    ) -> Result<PageFrame> {
        unreachable!("Not called")
    }
}

// Helper to create a new inode Arc.
fn new_inode() -> Arc<dyn Inode> {
    Arc::new(DummyTestInode)
}

// Creates a file-backed VMA for testing.
fn create_file_vma(
    start: usize,
    size: usize,
    perms: VMAPermissions,
    offset: u64,
    inode: Arc<dyn Inode>,
) -> VMArea {
    VMArea::new(
        VirtMemoryRegion::new(VA::from_value(start), size),
        VMAreaKind::File(VMFileMapping {
            file: inode,
            offset,
            len: size as u64,
        }),
        perms,
    )
}

// Creates an anonymous VMA for testing.
fn create_anon_vma(start: usize, size: usize, perms: VMAPermissions) -> VMArea {
    VMArea::new(
        VirtMemoryRegion::new(VA::from_value(start), size),
        VMAreaKind::Anon,
        perms,
    )
}

/// Asserts that a VMA with the given properties exists.
fn assert_vma_exists(pvm: &MemoryMap<MockAddressSpace>, start: usize, size: usize) {
    let vma = pvm
        .find_vma(VA::from_value(start))
        .expect("VMA not found at start address");
    assert_eq!(
        vma.region.start_address().value(),
        start,
        "VMA start address mismatch"
    );
    assert_eq!(vma.region.size(), size, "VMA size mismatch");
}

#[test]
fn test_mmap_any_empty() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let size = 3 * PAGE_SIZE;
    let addr = pvm
        .mmap(
            AddressRequest::Any,
            size,
            VMAPermissions::rw(),
            VMAreaKind::Anon,
        )
        .unwrap();

    assert_eq!(addr.value(), MMAP_BASE - size);
    assert_eq!(pvm.vmas.len(), 1);
    assert_vma_exists(&pvm, MMAP_BASE - size, size);
    assert!(pvm.address_space.ops_log.lock().unwrap().is_empty());
}

#[test]
fn test_mmap_any_with_existing() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let size = 2 * PAGE_SIZE;
    let existing_addr = MMAP_BASE - 5 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(existing_addr, size, VMAPermissions::rw()));

    // This should find the gap above the existing VMA.
    let new_addr = pvm
        .mmap(
            AddressRequest::Any,
            size,
            VMAPermissions::ro(),
            VMAreaKind::Anon,
        )
        .unwrap();

    assert_eq!(new_addr.value(), MMAP_BASE - size);
    assert_eq!(pvm.vmas.len(), 2);

    // This should find the gap below the existing VMA.
    let bottom_addr = pvm
        .mmap(
            AddressRequest::Any,
            size,
            VMAPermissions::ro(), // different permissions to prevent merge.
            VMAreaKind::Anon,
        )
        .unwrap();
    assert_eq!(bottom_addr.value(), existing_addr - size);
    assert_eq!(pvm.vmas.len(), 3);

    assert_vma_exists(&pvm, existing_addr, 2 * PAGE_SIZE);
    assert_vma_exists(&pvm, MMAP_BASE - 2 * PAGE_SIZE, 2 * PAGE_SIZE);
    assert_vma_exists(&pvm, MMAP_BASE - 7 * PAGE_SIZE, 2 * PAGE_SIZE);
    assert!(pvm.address_space.ops_log.lock().unwrap().is_empty());
}

#[test]
fn test_mmap_hint_free() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let size = PAGE_SIZE;
    let hint_addr = VA::from_value(MMAP_BASE - 10 * PAGE_SIZE);

    let addr = pvm
        .mmap(
            AddressRequest::Hint(hint_addr),
            size,
            VMAPermissions::rw(),
            VMAreaKind::Anon,
        )
        .unwrap();

    assert_eq!(addr, hint_addr);
    assert_vma_exists(&pvm, hint_addr.value(), size);
    assert!(pvm.address_space.ops_log.lock().unwrap().is_empty());
}

#[test]
fn test_mmap_hint_taken() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let size = 2 * PAGE_SIZE;
    let hint_addr = VA::from_value(MMAP_BASE - 10 * PAGE_SIZE);

    // Occupy the space where the hint is.
    pvm.insert_and_merge(create_anon_vma(
        hint_addr.value(),
        size,
        VMAPermissions::rw(),
    ));

    // The mmap should ignore the hint and find a new spot at the top.
    let new_addr = pvm
        .mmap(
            AddressRequest::Hint(hint_addr),
            size,
            VMAPermissions::rw(),
            VMAreaKind::Anon,
        )
        .unwrap();

    assert_ne!(new_addr, hint_addr);
    assert_eq!(new_addr.value(), MMAP_BASE - size);
    assert_eq!(pvm.vmas.len(), 2);
    assert!(pvm.address_space.ops_log.lock().unwrap().is_empty());
}

#[test]
fn test_mmap_fixed_clobber_complete_overlap() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;

    // Old VMA, read-only
    pvm.insert_and_merge(create_anon_vma(addr, 3 * PAGE_SIZE, VMAPermissions::ro()));

    // New VMA, completely overwrites the old one
    let mapped_addr = pvm
        .mmap(
            AddressRequest::Fixed {
                address: VA::from_value(addr),
                permit_overlap: true,
            },
            3 * PAGE_SIZE,
            VMAPermissions::rw(),
            VMAreaKind::Anon,
        )
        .unwrap();

    assert_eq!(mapped_addr.value(), addr);

    assert_eq!(pvm.vmas.len(), 1);
    let vma = pvm.find_vma(VA::from_value(addr)).unwrap();
    assert!(vma.permissions().write); // Check it's the new VMA
    assert_vma_exists(&pvm, addr, 3 * PAGE_SIZE);
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[MockPageTableOp::ProtectRange {
            region: VirtMemoryRegion::new(VA::from_value(addr), 3 * PAGE_SIZE),
            perms: PtePermissions::rw(true)
        }]
    );
}

#[test]
fn test_mmap_fixed_clobber_partial_end() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(addr, 5 * PAGE_SIZE, VMAPermissions::ro()));

    // New VMA overwrites the end of the old one.
    let new_addr = addr + 3 * PAGE_SIZE;
    let new_size = 2 * PAGE_SIZE;
    pvm.mmap(
        AddressRequest::Fixed {
            address: VA::from_value(new_addr),
            permit_overlap: true,
        },
        new_size,
        VMAPermissions::rw(),
        VMAreaKind::Anon,
    )
    .unwrap();

    assert_eq!(pvm.vmas.len(), 2);
    assert_vma_exists(&pvm, addr, 3 * PAGE_SIZE); // Original is truncated
    assert_vma_exists(&pvm, new_addr, new_size); // New VMA exists
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[MockPageTableOp::ProtectRange {
            region: VirtMemoryRegion::new(VA::from_value(new_addr), 2 * PAGE_SIZE),
            perms: PtePermissions::rw(true),
        }]
    );
}

#[test]
fn test_mmap_fixed_clobber_partial_end_spill() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(addr, 5 * PAGE_SIZE, VMAPermissions::ro()));

    // New VMA overwrites the end of the old one.
    let new_addr = addr + 3 * PAGE_SIZE;
    let new_size = 4 * PAGE_SIZE;
    pvm.mmap(
        AddressRequest::Fixed {
            address: VA::from_value(new_addr),
            permit_overlap: true,
        },
        new_size,
        VMAPermissions::rw(),
        VMAreaKind::Anon,
    )
    .unwrap();

    assert_eq!(pvm.vmas.len(), 2);
    assert_vma_exists(&pvm, addr, 3 * PAGE_SIZE); // Original is truncated
    assert_vma_exists(&pvm, new_addr, new_size); // New VMA exists

    // Ensure protect region is just the overlapping region.
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[MockPageTableOp::ProtectRange {
            region: VirtMemoryRegion::new(VA::from_value(new_addr), 2 * PAGE_SIZE),
            perms: PtePermissions::rw(true),
        }]
    );
}

#[test]
fn test_mmap_fixed_no_clobber_fails() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(addr, 5 * PAGE_SIZE, VMAPermissions::ro()));

    let new_addr = addr + 3 * PAGE_SIZE;
    let new_size = 2 * PAGE_SIZE;
    assert!(
        pvm.mmap(
            AddressRequest::Fixed {
                address: VA::from_value(new_addr),
                permit_overlap: false,
            },
            new_size,
            VMAPermissions::rw(),
            VMAreaKind::Anon,
        )
        .is_err()
    );
}

#[test]
fn test_mmap_fixed_clobber_punch_hole() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;

    // A large VMA
    pvm.insert_and_merge(create_anon_vma(addr, 10 * PAGE_SIZE, VMAPermissions::rw()));

    // A new VMA is mapped right in the middle.
    let new_addr = addr + 3 * PAGE_SIZE;
    let new_size = 4 * PAGE_SIZE;
    // Use different perms to prevent merging.
    pvm.mmap(
        AddressRequest::Fixed {
            address: VA::from_value(new_addr),
            permit_overlap: true,
        },
        new_size,
        VMAPermissions::ro(),
        VMAreaKind::Anon,
    )
    .unwrap();

    assert_eq!(pvm.vmas.len(), 3);
    // Left part of the original VMA
    assert_vma_exists(&pvm, addr, 3 * PAGE_SIZE);
    // The new VMA
    assert_vma_exists(&pvm, new_addr, new_size);
    // Right part of the original VMA
    assert_vma_exists(&pvm, new_addr + new_size, 3 * PAGE_SIZE);
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[MockPageTableOp::ProtectRange {
            region: VirtMemoryRegion::new(VA::from_value(new_addr), new_size),
            perms: PtePermissions::ro(true),
        }]
    );
}

#[test]
fn test_merge_with_previous_and_next() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let perms = VMAPermissions::rw();
    let addr1 = MMAP_BASE - 20 * PAGE_SIZE;
    let addr2 = addr1 + 5 * PAGE_SIZE;
    let addr3 = addr2 + 5 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(addr1, 5 * PAGE_SIZE, perms));
    pvm.insert_and_merge(create_anon_vma(addr3, 5 * PAGE_SIZE, perms));

    assert_eq!(pvm.vmas.len(), 2);

    // Insert the middle part, which should merge with both.
    pvm.insert_and_merge(create_anon_vma(addr2, 5 * PAGE_SIZE, perms));

    assert_eq!(pvm.vmas.len(), 1);
    assert_vma_exists(&pvm, addr1, 15 * PAGE_SIZE);
    assert!(pvm.address_space.ops_log.lock().unwrap().is_empty());
}

#[test]
fn test_merge_with_smaller_region() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let perms = VMAPermissions::rw();
    let addr = MMAP_BASE - 20 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(addr, 5 * PAGE_SIZE, perms));
    pvm.insert_and_merge(create_anon_vma(addr + 5 * PAGE_SIZE, PAGE_SIZE, perms));

    assert_eq!(pvm.vmas.len(), 1);
    assert_vma_exists(&pvm, addr, 6 * PAGE_SIZE);
}

#[test]
fn test_merge_with_same_sz_region() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let perms = VMAPermissions::rw();
    let addr = MMAP_BASE - 20 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(addr, 5 * PAGE_SIZE, perms));
    pvm.insert_and_merge(create_anon_vma(addr + 5 * PAGE_SIZE, 5 * PAGE_SIZE, perms));

    assert_eq!(pvm.vmas.len(), 1);
    assert_vma_exists(&pvm, addr, 10 * PAGE_SIZE);
}

#[test]
fn test_merge_with_larger_region() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let perms = VMAPermissions::rw();
    let addr = MMAP_BASE - 20 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(addr, 5 * PAGE_SIZE, perms));
    pvm.insert_and_merge(create_anon_vma(addr + 5 * PAGE_SIZE, 10 * PAGE_SIZE, perms));

    assert_eq!(pvm.vmas.len(), 1);
    assert_vma_exists(&pvm, addr, 15 * PAGE_SIZE);
}

#[test]
fn test_merge_file_backed_contiguous() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let perms = VMAPermissions::rw();
    let inode = new_inode();
    let addr1 = MMAP_BASE - 10 * PAGE_SIZE;
    let size1 = 2 * PAGE_SIZE;
    let offset1 = 0;

    let addr2 = addr1 + size1;
    let size2 = 3 * PAGE_SIZE;
    let offset2 = offset1 + size1 as u64;

    // Insert two contiguous, file-backed VMAs. They should merge.
    pvm.insert_and_merge(create_file_vma(
        addr1,
        size1,
        perms,
        offset1,
        Arc::clone(&inode),
    ));
    pvm.insert_and_merge(create_file_vma(
        addr2,
        size2,
        perms,
        offset2,
        Arc::clone(&inode),
    ));

    assert_eq!(pvm.vmas.len(), 1);
    assert_vma_exists(&pvm, addr1, size1 + size2);
    let vma = pvm.find_vma(VA::from_value(addr1)).unwrap();
    match &vma.kind {
        VMAreaKind::File(fm) => assert_eq!(fm.offset, offset1),
        _ => panic!("Expected file-backed VMA"),
    }
    assert!(pvm.address_space.ops_log.lock().unwrap().is_empty());
}

#[test]
fn test_no_merge_file_backed_non_contiguous() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let perms = VMAPermissions::rw();
    let inode = new_inode();
    let addr1 = MMAP_BASE - 10 * PAGE_SIZE;
    let size1 = 2 * PAGE_SIZE;
    let offset1 = 0;

    let addr2 = addr1 + size1;
    let size2 = 3 * PAGE_SIZE;
    let offset2 = offset1 + size1 as u64 + 123; // Non-contiguous offset!

    pvm.insert_and_merge(create_file_vma(
        addr1,
        size1,
        perms,
        offset1,
        Arc::clone(&inode),
    ));
    pvm.insert_and_merge(create_file_vma(
        addr2,
        size2,
        perms,
        offset2,
        Arc::clone(&inode),
    ));

    assert_eq!(pvm.vmas.len(), 2); // Should not merge
    assert_vma_exists(&pvm, addr1, size1);
    assert_vma_exists(&pvm, addr2, size2);
    assert!(pvm.address_space.ops_log.lock().unwrap().is_empty());
}

#[test]
fn test_munmap_full_vma() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;
    let size = 5 * PAGE_SIZE;
    let region = VirtMemoryRegion::new(VA::from_value(addr), size);
    pvm.insert_and_merge(create_anon_vma(addr, size, VMAPermissions::rw()));

    assert_eq!(pvm.vmas.len(), 1);
    pvm.munmap(region).unwrap();
    assert!(pvm.vmas.is_empty());
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[MockPageTableOp::UnmapRange { region: region }]
    );
}

#[test]
fn test_munmap_truncate_start() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;
    let size = 5 * PAGE_SIZE;
    pvm.insert_and_merge(create_anon_vma(addr, size, VMAPermissions::rw()));

    let unmap_size = 2 * PAGE_SIZE;

    let region = VirtMemoryRegion::new(VA::from_value(addr), unmap_size);
    pvm.munmap(region).unwrap();

    assert_eq!(pvm.vmas.len(), 1);
    let new_start = addr + unmap_size;
    let new_size = size - unmap_size;
    assert_vma_exists(&pvm, new_start, new_size);
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[MockPageTableOp::UnmapRange { region: region }]
    );
}

#[test]
fn test_munmap_truncate_end() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;
    let size = 5 * PAGE_SIZE;
    pvm.insert_and_merge(create_anon_vma(addr, size, VMAPermissions::rw()));

    // Unmap the last two pages
    let unmap_size = 2 * PAGE_SIZE;
    let region = VirtMemoryRegion::new(VA::from_value(addr + (size - unmap_size)), unmap_size);
    pvm.munmap(region).unwrap();

    assert_eq!(pvm.vmas.len(), 1);
    let new_size = size - unmap_size;
    assert_vma_exists(&pvm, addr, new_size);
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[MockPageTableOp::UnmapRange { region: region }]
    );
}

#[test]
fn test_munmap_punch_hole() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr = MMAP_BASE - 10 * PAGE_SIZE;
    let size = 10 * PAGE_SIZE;
    pvm.insert_and_merge(create_anon_vma(addr, size, VMAPermissions::rw()));

    // Unmap a 4-page hole in the middle
    let unmap_start = addr + 3 * PAGE_SIZE;
    let unmap_size = 4 * PAGE_SIZE;
    let region = VirtMemoryRegion::new(VA::from_value(unmap_start), unmap_size);
    pvm.munmap(region).unwrap();

    assert_eq!(pvm.vmas.len(), 2);
    // Left part
    assert_vma_exists(&pvm, addr, 3 * PAGE_SIZE);
    // Right part
    let right_start = unmap_start + unmap_size;
    let right_size = 3 * PAGE_SIZE;
    assert_vma_exists(&pvm, right_start, right_size);
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[MockPageTableOp::UnmapRange { region: region }]
    );
}

#[test]
fn test_munmap_over_multiple_vmas() {
    let mut pvm: MemoryMap<MockAddressSpace> = MemoryMap::new().unwrap();
    let addr1 = MMAP_BASE - 20 * PAGE_SIZE;
    let addr2 = addr1 + 5 * PAGE_SIZE;
    let addr3 = addr2 + 5 * PAGE_SIZE;

    pvm.insert_and_merge(create_anon_vma(addr1, 3 * PAGE_SIZE, VMAPermissions::rw()));
    pvm.insert_and_merge(create_anon_vma(addr2, 3 * PAGE_SIZE, VMAPermissions::rw()));
    pvm.insert_and_merge(create_anon_vma(addr3, 3 * PAGE_SIZE, VMAPermissions::rw()));
    assert_eq!(pvm.vmas.len(), 3);

    // Unmap from the middle of the first VMA to the middle of the last one.
    let unmap_start = addr1 + PAGE_SIZE;
    let unmap_end = addr3 + 2 * PAGE_SIZE;
    let unmap_len = unmap_end - unmap_start;
    let region = VirtMemoryRegion::new(VA::from_value(unmap_start), unmap_len);

    pvm.munmap(region).unwrap();
    assert_eq!(pvm.vmas.len(), 2);

    // First VMA is truncated at the end
    assert_vma_exists(&pvm, addr1, PAGE_SIZE);
    // Last VMA is truncated at the start
    assert_vma_exists(&pvm, unmap_end, PAGE_SIZE);
    assert_eq!(
        *pvm.address_space.ops_log.lock().unwrap(),
        &[
            MockPageTableOp::UnmapRange {
                region: VirtMemoryRegion::new(VA::from_value(addr1 + PAGE_SIZE), 2 * PAGE_SIZE)
            },
            MockPageTableOp::UnmapRange {
                region: VirtMemoryRegion::new(VA::from_value(addr2), 3 * PAGE_SIZE)
            },
            MockPageTableOp::UnmapRange {
                region: VirtMemoryRegion::new(VA::from_value(addr3), 2 * PAGE_SIZE)
            },
        ]
    );
}
