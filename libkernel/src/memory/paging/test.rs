//! Test harness for paging unit-tests.

use crate::{
    error::KernelError,
    memory::address::{IdentityTranslator, TPA, TVA},
};

use super::{
    PageAllocator, PageTableMapper, PgTable, PgTableArray, TLBInvalidator, TranslationChange,
    walk::WalkContext,
};

/// A mock TLB invalidator that does nothing for unit testing.
pub struct MockTLBInvalidator;
impl TLBInvalidator for MockTLBInvalidator {
    fn prepare(&self, _change: &TranslationChange) {}
    fn invalidate(&self, _change: &TranslationChange) {}
    fn publish(&self, _change: &TranslationChange) {}
}

/// Mock page allocator that allocates on the host heap and uses a counter
/// to simulate memory limits.
pub struct MockPageAllocator {
    pub pages_allocated: usize,
    pub max_pages: usize,
}

impl MockPageAllocator {
    fn new(max_pages: usize) -> Self {
        Self {
            pages_allocated: 0,
            max_pages,
        }
    }
}

impl PageAllocator for MockPageAllocator {
    fn allocate_page_table<T: PgTable>(&mut self) -> crate::error::Result<TPA<PgTableArray<T>>> {
        if self.pages_allocated >= self.max_pages {
            Err(KernelError::NoMemory)
        } else {
            self.pages_allocated += 1;
            // Allocate a page-aligned table on the host heap.
            let layout = std::alloc::Layout::new::<PgTableArray<T>>();

            let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            if ptr.is_null() {
                panic!("Host failed to allocate memory for test");
            }

            // Return the raw pointer value as our "physical address".
            Ok(TPA::from_value(ptr as usize))
        }
    }
}

/// A mock mapper for host-based testing. It assumes that the "physical
/// address" (TPA) is just a raw pointer from the host's virtual address
/// space, which is true for tests using heap allocation. It performs a
/// direct cast.
pub struct PassthroughMapper;

impl PageTableMapper for PassthroughMapper {
    unsafe fn with_page_table<T: PgTable, R>(
        &mut self,
        pa: TPA<PgTableArray<T>>,
        f: impl FnOnce(TVA<PgTableArray<T>>) -> R,
    ) -> crate::error::Result<R> {
        // The "physical address" in our test is the raw pointer from the heap.
        // Just cast it back and use it.
        Ok(f(pa.to_va::<IdentityTranslator>()))
    }
}

pub struct TestHarness<R: PgTable> {
    pub allocator: MockPageAllocator,
    pub mapper: PassthroughMapper,
    pub invalidator: MockTLBInvalidator,
    pub root_table: TPA<PgTableArray<R>>,
}

impl<R: PgTable> TestHarness<R> {
    pub fn new(max_pages: usize) -> Self {
        let mut allocator = MockPageAllocator::new(max_pages);
        let root_table = allocator.allocate_page_table::<R>().unwrap();

        Self {
            allocator,
            mapper: PassthroughMapper,
            invalidator: MockTLBInvalidator,
            root_table,
        }
    }

    pub fn create_walk_ctx(&mut self) -> WalkContext<'_, PassthroughMapper, MockTLBInvalidator> {
        WalkContext {
            mapper: &mut self.mapper,
            invalidator: &self.invalidator,
        }
    }
}

macro_rules! block_split_tests {
    ($middle:ty, $small:ty, $leaf:ty, $big_desc:ty, $small_desc:ty, $page_desc:ty, $mem:expr) => {
        fn read_table<
            T: PgTable,
            P: PgTable<Descriptor: crate::memory::paging::TableMapper<NextLevel = T>>,
        >(
            harness: &mut TestHarness,
            parent: TPA<PgTableArray<P>>,
            va: VA,
        ) -> TPA<PgTableArray<T>> {
            use crate::memory::paging::TableMapper;
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(parent, |ptr| {
                        P::from_ptr(ptr).get_desc(va).next_table_address().unwrap()
                    })
                    .unwrap()
            }
        }

        #[test]
        fn punch_blocks_preserves_surroundings() {
            use crate::memory::paging::PaMapper;
            for (large, hole_idx) in [
                (false, 0),
                (false, 257),
                (false, 511),
                (true, 0),
                (true, 257),
                (true, 511),
            ] {
                let mut harness = TestHarness::new(10);
                let va = VA::from_value(0x4_0000_0000);
                let pa = PA::from_value(0x8000_0000);
                let root = harness.inner.root_table;
                let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
                let perms = PtePermissions::ro(true);
                let original = if large {
                    let desc = <$big_desc>::new_map_pa(pa, $mem, perms);
                    unsafe {
                        harness
                            .inner
                            .mapper
                            .with_page_table(middle, |ptr| {
                                <$middle>::from_ptr(ptr).set_desc(
                                    va,
                                    desc,
                                    &harness.inner.invalidator,
                                );
                            })
                            .unwrap();
                    }
                    desc.as_raw()
                } else {
                    let small = map_at_level(middle, va, &mut harness.create_map_ctx()).unwrap();
                    let desc = <$small_desc>::new_map_pa(pa, $mem, perms);
                    unsafe {
                        harness
                            .inner
                            .mapper
                            .with_page_table(small, |ptr| {
                                <$small>::from_ptr(ptr).set_desc(
                                    va,
                                    desc,
                                    &harness.inner.invalidator,
                                );
                            })
                            .unwrap();
                    }
                    desc.as_raw()
                };
                let before = harness.inner.allocator.pages_allocated;
                let hole = va.add_pages(hole_idx);
                punch_hole(
                    root,
                    VirtMemoryRegion::new(hole, PAGE_SIZE),
                    &mut harness.create_modify_ctx(),
                )
                .unwrap();
                assert_eq!(
                    harness.inner.allocator.pages_allocated - before,
                    if large { 2 } else { 1 }
                );
                let small = read_table::<$small, $middle>(&mut harness, middle, va);
                let leaf = read_table::<$leaf, $small>(&mut harness, small, va);
                let parent = if large {
                    <$small_desc>::new_mapping(
                        pa,
                        <$big_desc>::from_raw(original)
                            .mapping_attributes()
                            .unwrap(),
                    )
                } else {
                    <$small_desc>::from_raw(original)
                };
                unsafe {
                    harness
                        .inner
                        .mapper
                        .with_page_table(leaf, |ptr| {
                            let table = <$leaf>::from_ptr(ptr);
                            for idx in 0..512 {
                                let expected = if idx == hole_idx {
                                    <$page_desc>::invalid()
                                } else {
                                    <$page_desc>::new_mapping(
                                        pa.add_pages(idx),
                                        parent.mapping_attributes().unwrap(),
                                    )
                                };
                                assert_eq!(table.get_idx(idx).as_raw(), expected.as_raw());
                            }
                        })
                        .unwrap();
                    if large {
                        harness
                            .inner
                            .mapper
                            .with_page_table(small, |ptr| {
                                let table = <$small>::from_ptr(ptr);
                                for idx in 1..512 {
                                    assert_eq!(
                                        table.get_idx(idx).as_raw(),
                                        <$small_desc>::new_mapping(
                                            pa.add_bytes(idx << 21),
                                            <$big_desc>::from_raw(original)
                                                .mapping_attributes()
                                                .unwrap(),
                                        )
                                        .as_raw()
                                    );
                                }
                            })
                            .unwrap();
                    }
                }
                let allocated = harness.inner.allocator.pages_allocated;
                walk_and_modify_region(
                    root,
                    VirtMemoryRegion::new(hole, PAGE_SIZE),
                    &mut harness.create_walk_ctx(),
                    |_, _| panic!("Hole must be skipped"),
                )
                .unwrap();
                assert_eq!(harness.inner.allocator.pages_allocated, allocated);
            }
        }

        #[test]
        fn split_allocation_failure_preserves_block() {
            let mut harness = TestHarness::new(3);
            let va = VA::from_value(0x4_0000_0000);
            let root = harness.inner.root_table;
            let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
            let small = map_at_level(middle, va, &mut harness.create_map_ctx()).unwrap();
            let desc = <$small_desc>::new_map_pa(
                PA::from_value(0x8000_0000),
                $mem,
                PtePermissions::rw(false),
            );
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(small, |ptr| {
                        <$small>::from_ptr(ptr).set_desc(va, desc, &harness.inner.invalidator);
                    })
                    .unwrap();
            }

            let result = punch_hole(
                root,
                VirtMemoryRegion::new(va.add_pages(1), PAGE_SIZE),
                &mut harness.create_modify_ctx(),
            );
            assert!(matches!(result, Err(crate::error::KernelError::NoMemory)));
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(small, |ptr| {
                        assert_eq!(<$small>::from_ptr(ptr).get_desc(va).as_raw(), desc.as_raw());
                    })
                    .unwrap();
            }
        }

        #[test]
        fn punch_across_blocks_reports_full_invalidation_ranges() {
            use crate::memory::paging::{TLBInvalidator, TableMapper, TranslationChange};
            use std::cell::RefCell;

            struct RecordingInvalidator(RefCell<Vec<TranslationChange>>);
            impl TLBInvalidator for RecordingInvalidator {
                fn prepare(&self, _change: &TranslationChange) {}
                fn invalidate(&self, change: &TranslationChange) {
                    self.0.borrow_mut().push(*change);
                }
                fn publish(&self, _change: &TranslationChange) {}
            }
            let mut harness = TestHarness::new(8);
            let va = VA::from_value(0x4_0000_0000);
            let pa = PA::from_value(0x8000_0000);
            let root = harness.inner.root_table;
            let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(middle, |ptr| {
                        for idx in 0..2 {
                            <$middle>::from_ptr(ptr).set_desc(
                                va.add_bytes(idx << 30),
                                <$big_desc>::new_map_pa(
                                    pa.add_bytes(idx << 30),
                                    $mem,
                                    PtePermissions::ro(true),
                                ),
                                &harness.inner.invalidator,
                            );
                        }
                    })
                    .unwrap();
            }
            let invalidator = RecordingInvalidator(RefCell::new(Vec::new()));
            let hole = VA::from_value(va.value() + (1 << 30) - PAGE_SIZE);
            let ctx = harness.create_map_ctx();
            let mut ctx = ModifyContext {
                allocator: ctx.allocator,
                mapper: ctx.mapper,
                invalidator: &invalidator,
            };
            punch_hole(root, VirtMemoryRegion::new(hole, 2 * PAGE_SIZE), &mut ctx).unwrap();
            assert_eq!(harness.inner.allocator.pages_allocated, 6);
            let changes = invalidator.0.borrow();
            assert_eq!(
                changes.iter().map(|c| c.size).collect::<Vec<_>>(),
                [1 << 30, 1 << 21, PAGE_SIZE, 1 << 30, 1 << 21, PAGE_SIZE]
            );
            for change in changes.iter() {
                let mapped_pa = pa.add_bytes(change.va.value() - va.value());
                let perms = PtePermissions::ro(true);
                let (old, new) = if change.size == PAGE_SIZE {
                    (
                        <$page_desc>::new_map_pa(mapped_pa, $mem, perms).as_raw(),
                        <$page_desc>::invalid().as_raw(),
                    )
                } else {
                    let small = read_table::<$small, $middle>(&mut harness, middle, change.va);
                    if change.size == 1 << 21 {
                        let leaf = read_table::<$leaf, $small>(&mut harness, small, change.va);
                        (
                            <$small_desc>::new_map_pa(mapped_pa, $mem, perms).as_raw(),
                            <$small_desc>::new_next_table(leaf).as_raw(),
                        )
                    } else {
                        (
                            <$big_desc>::new_map_pa(mapped_pa, $mem, perms).as_raw(),
                            <$big_desc>::new_next_table(small).as_raw(),
                        )
                    }
                };
                assert_eq!(change.old_descriptor, old);
                assert_eq!(change.new_descriptor, new);
                assert_eq!(change.va.value() & (change.size - 1), 0);
            }
            assert_eq!(changes[0].va, va);
            assert_eq!(changes[3].va, va.add_bytes(1 << 30));
            for idx in 0..2 {
                let address = if idx == 0 { hole } else { hole.add_pages(1) };
                let small = read_table::<$small, $middle>(&mut harness, middle, address);
                let leaf = read_table::<$leaf, $small>(&mut harness, small, address);
                unsafe {
                    harness
                        .inner
                        .mapper
                        .with_page_table(leaf, |ptr| {
                            let table = <$leaf>::from_ptr(ptr);
                            assert!(!table.get_desc(address).is_valid());
                            let neighbor = if idx == 0 {
                                VA::from_value(address.value() - PAGE_SIZE)
                            } else {
                                address.add_pages(1)
                            };
                            assert_eq!(
                                table.get_desc(neighbor).mapped_address().unwrap().value(),
                                pa.value() + neighbor.value() - va.value()
                            );
                        })
                        .unwrap();
                }
            }
        }

        #[test]
        fn punch_whole_blocks_without_allocating() {
            use crate::memory::paging::{TLBInvalidator, TranslationChange};
            use std::cell::RefCell;

            struct Recorder(RefCell<Vec<TranslationChange>>);
            impl TLBInvalidator for Recorder {
                fn prepare(&self, _: &TranslationChange) {}
                fn invalidate(&self, change: &TranslationChange) {
                    self.0.borrow_mut().push(*change);
                }
                fn publish(&self, _: &TranslationChange) {}
            }
            for large in [false, true] {
                let mut harness = TestHarness::new(if large { 2 } else { 3 });
                let va = VA::from_value(0x4_0000_0000);
                let pa = PA::from_value(0x8000_0000);
                let root = harness.inner.root_table;
                let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
                let size = if large { 1 << 30 } else { 1 << 21 };
                let small = if large {
                    None
                } else {
                    Some(map_at_level(middle, va, &mut harness.create_map_ctx()).unwrap())
                };
                unsafe {
                    if let Some(small) = small {
                        harness
                            .inner
                            .mapper
                            .with_page_table(small, |ptr| {
                                for idx in 0..3 {
                                    <$small>::from_ptr(ptr).set_desc(
                                        va.add_bytes(idx * size),
                                        <$small_desc>::new_map_pa(
                                            pa.add_bytes(idx * size),
                                            $mem,
                                            PtePermissions::ro(true),
                                        ),
                                        &harness.inner.invalidator,
                                    );
                                }
                            })
                            .unwrap();
                    } else {
                        harness
                            .inner
                            .mapper
                            .with_page_table(middle, |ptr| {
                                for idx in 0..3 {
                                    <$middle>::from_ptr(ptr).set_desc(
                                        va.add_bytes(idx * size),
                                        <$big_desc>::new_map_pa(
                                            pa.add_bytes(idx * size),
                                            $mem,
                                            PtePermissions::ro(true),
                                        ),
                                        &harness.inner.invalidator,
                                    );
                                }
                            })
                            .unwrap();
                    }
                }
                let allocated = harness.inner.allocator.pages_allocated;
                let recorder = Recorder(RefCell::new(Vec::new()));
                let ctx = harness.create_map_ctx();
                let mut ctx = ModifyContext {
                    allocator: ctx.allocator,
                    mapper: ctx.mapper,
                    invalidator: &recorder,
                };
                punch_hole(
                    root,
                    VirtMemoryRegion::new(va.add_bytes(size), size),
                    &mut ctx,
                )
                .unwrap();
                assert_eq!(harness.inner.allocator.pages_allocated, allocated);
                let changes = recorder.0.borrow();
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].va, va.add_bytes(size));
                assert_eq!(changes[0].size, size);
                let old = if large {
                    <$big_desc>::new_map_pa(pa.add_bytes(size), $mem, PtePermissions::ro(true))
                        .as_raw()
                } else {
                    <$small_desc>::new_map_pa(pa.add_bytes(size), $mem, PtePermissions::ro(true))
                        .as_raw()
                };
                assert_eq!(changes[0].old_descriptor, old);
                assert_eq!(changes[0].new_descriptor, 0);
                unsafe {
                    if let Some(small) = small {
                        harness
                            .inner
                            .mapper
                            .with_page_table(small, |ptr| {
                                let table = <$small>::from_ptr(ptr);
                                for idx in 0..3 {
                                    let expected = if idx == 1 {
                                        <$small_desc>::invalid()
                                    } else {
                                        <$small_desc>::new_map_pa(
                                            pa.add_bytes(idx * size),
                                            $mem,
                                            PtePermissions::ro(true),
                                        )
                                    };
                                    assert_eq!(
                                        table.get_desc(va.add_bytes(idx * size)).as_raw(),
                                        expected.as_raw()
                                    );
                                }
                            })
                            .unwrap();
                    } else {
                        harness
                            .inner
                            .mapper
                            .with_page_table(middle, |ptr| {
                                let table = <$middle>::from_ptr(ptr);
                                for idx in 0..3 {
                                    let expected = if idx == 1 {
                                        <$big_desc>::invalid()
                                    } else {
                                        <$big_desc>::new_map_pa(
                                            pa.add_bytes(idx * size),
                                            $mem,
                                            PtePermissions::ro(true),
                                        )
                                    };
                                    assert_eq!(
                                        table.get_desc(va.add_bytes(idx * size)).as_raw(),
                                        expected.as_raw()
                                    );
                                }
                            })
                            .unwrap();
                    }
                }
            }
        }

        #[test]
        fn punch_partial_gigabyte_demotes_only_boundary_blocks() {
            let mut harness = TestHarness::new(5);
            let root = harness.inner.root_table;
            let va = VA::from_value(0xffff_8000_0000_0000);
            let pa = PA::from_value(0x8000_0000);
            let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
            let original = <$big_desc>::new_map_pa(pa, $mem, PtePermissions::ro(true));
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(middle, |ptr| {
                        <$middle>::from_ptr(ptr).set_desc(va, original, &harness.inner.invalidator);
                    })
                    .unwrap();
            }
            // Keep one page at each end. All 510 interior 2 MiB blocks should
            // be removed directly, leaving just the two boundary page tables.
            let hole = VirtMemoryRegion::new(va.add_pages(1), (1 << 30) - 2 * PAGE_SIZE);
            punch_hole(root, hole, &mut harness.create_modify_ctx()).unwrap();
            assert_eq!(harness.inner.allocator.pages_allocated, 5);
            let small = read_table::<$small, $middle>(&mut harness, middle, va);
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(small, |ptr| {
                        for idx in 1..511 {
                            assert!(!<$small>::from_ptr(ptr).get_idx(idx).is_valid());
                        }
                    })
                    .unwrap();
            }
            for idx in [0, 511] {
                let base = va.add_bytes(idx << 21);
                let leaf = read_table::<$leaf, $small>(&mut harness, small, base);
                unsafe {
                    harness
                        .inner
                        .mapper
                        .with_page_table(leaf, |ptr| {
                            for page in 0..512 {
                                let desc = <$leaf>::from_ptr(ptr).get_idx(page);
                                if (idx, page) == (0, 0) || (idx, page) == (511, 511) {
                                    assert_eq!(
                                        desc.mapped_address(),
                                        Some(pa.add_bytes((idx << 21) + page * PAGE_SIZE))
                                    );
                                    assert_eq!(
                                        desc.mapping_attributes(),
                                        original.mapping_attributes()
                                    );
                                } else {
                                    assert!(!desc.is_valid());
                                }
                            }
                        })
                        .unwrap();
                }
            }
            punch_hole(root, hole, &mut harness.create_modify_ctx()).unwrap();
            assert_eq!(harness.inner.allocator.pages_allocated, 5);
            punch_hole(
                root,
                VirtMemoryRegion::new(va, 1 << 30),
                &mut harness.create_modify_ctx(),
            )
            .unwrap();
            assert_eq!(harness.inner.allocator.pages_allocated, 5);
            for idx in [0, 511] {
                let leaf =
                    read_table::<$leaf, $small>(&mut harness, small, va.add_bytes(idx << 21));
                unsafe {
                    harness
                        .inner
                        .mapper
                        .with_page_table(leaf, |ptr| {
                            for page in 0..512 {
                                assert!(!<$leaf>::from_ptr(ptr).get_idx(page).is_valid());
                            }
                        })
                        .unwrap();
                }
            }
        }

        #[test]
        fn punch_aligned_subblock_needs_only_one_table() {
            let mut harness = TestHarness::new(3);
            let root = harness.inner.root_table;
            let va = VA::from_value(0x4_0000_0000);
            let pa = PA::from_value(0x8000_0000);
            let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
            let original = <$big_desc>::new_map_pa(pa, $mem, PtePermissions::ro(true));
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(middle, |ptr| {
                        <$middle>::from_ptr(ptr).set_desc(va, original, &harness.inner.invalidator);
                    })
                    .unwrap();
            }
            punch_hole(
                root,
                VirtMemoryRegion::new(va.add_bytes(123 << 21), 1 << 21),
                &mut harness.create_modify_ctx(),
            )
            .unwrap();
            assert_eq!(harness.inner.allocator.pages_allocated, 3);
            let small = read_table::<$small, $middle>(&mut harness, middle, va);
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(small, |ptr| {
                        for idx in 0..512 {
                            let desc = <$small>::from_ptr(ptr).get_idx(idx);
                            if idx == 123 {
                                assert!(!desc.is_valid());
                            } else {
                                assert_eq!(desc.mapped_address(), Some(pa.add_bytes(idx << 21)));
                                assert_eq!(
                                    desc.mapping_attributes(),
                                    original.mapping_attributes()
                                );
                            }
                        }
                    })
                    .unwrap();
            }
            // A subsequent partial-page request cannot allocate another table.
            let result = punch_hole(
                root,
                VirtMemoryRegion::new(va.add_pages(1), PAGE_SIZE),
                &mut harness.create_modify_ctx(),
            );
            assert!(matches!(result, Err(crate::error::KernelError::NoMemory)));
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(small, |ptr| {
                        assert_eq!(
                            <$small>::from_ptr(ptr).get_idx(0).mapped_address(),
                            Some(pa)
                        );
                    })
                    .unwrap();
            }
        }

        #[test]
        fn punch_at_top_of_address_space_preserves_final_page() {
            use crate::memory::paging::PaMapper;

            // Final page of the 64-bit address space.
            const LAST_PAGE: usize = usize::MAX - PAGE_SIZE + 1;

            for large in [false, true] {
                let block_size: usize = if large { 1 << 30 } else { 1 << 21 };
                // A block occupying the very top of the address space, so its
                // descriptor is the last entry of its table.
                let va = VA::from_value(usize::MAX - block_size + 1);
                let pa = PA::from_value(0x8000_0000);
                let perms = PtePermissions::ro(true);

                let mut harness = TestHarness::new(16);
                let root = harness.inner.root_table;
                let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
                let attrs = if large {
                    let desc = <$big_desc>::new_map_pa(pa, $mem, perms);
                    unsafe {
                        harness
                            .inner
                            .mapper
                            .with_page_table(middle, |ptr| {
                                <$middle>::from_ptr(ptr).set_desc(
                                    va,
                                    desc,
                                    &harness.inner.invalidator,
                                );
                            })
                            .unwrap();
                    }
                    desc.mapping_attributes().unwrap()
                } else {
                    let small = map_at_level(middle, va, &mut harness.create_map_ctx()).unwrap();
                    let desc = <$small_desc>::new_map_pa(pa, $mem, perms);
                    unsafe {
                        harness
                            .inner
                            .mapper
                            .with_page_table(small, |ptr| {
                                <$small>::from_ptr(ptr).set_desc(
                                    va,
                                    desc,
                                    &harness.inner.invalidator,
                                );
                            })
                            .unwrap();
                    }
                    desc.mapping_attributes().unwrap()
                };

                // Punch the whole block except its final page. The block is
                // therefore *not* fully covered and must be split, not zapped.
                punch_hole(
                    root,
                    VirtMemoryRegion::new(va, block_size - PAGE_SIZE),
                    &mut harness.create_modify_ctx(),
                )
                .unwrap();

                // The final page must survive, still mapping its original frame.
                let survivor = get_pte(root, VA::from_value(LAST_PAGE), &mut harness.inner.mapper)
                    .unwrap()
                    .expect("final page must remain mapped");
                assert_eq!(
                    survivor.as_raw(),
                    <$page_desc>::new_mapping(pa.add_bytes(block_size - PAGE_SIZE), attrs).as_raw(),
                    "final page was altered (large={large})"
                );

                // Everything below it in the block must be gone.
                for offset in [0, PAGE_SIZE, block_size - (2 * PAGE_SIZE)] {
                    assert!(
                        get_pte(root, va.add_bytes(offset), &mut harness.inner.mapper)
                            .unwrap()
                            .is_none(),
                        "offset {offset:#x} should have been punched (large={large})"
                    );
                }
            }
        }

        /// Punching a single page out of the last block must split it, leaving
        /// every other page in the block intact.
        #[test]
        fn punch_single_page_in_top_block_preserves_surroundings() {
            use crate::memory::paging::PaMapper;

            let block_size: usize = 1 << 21;
            let va = VA::from_value(usize::MAX - block_size + 1);
            let pa = PA::from_value(0x8000_0000);
            let perms = PtePermissions::ro(true);

            let mut harness = TestHarness::new(16);
            let root = harness.inner.root_table;
            let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
            let small = map_at_level(middle, va, &mut harness.create_map_ctx()).unwrap();
            let desc = <$small_desc>::new_map_pa(pa, $mem, perms);
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(small, |ptr| {
                        <$small>::from_ptr(ptr).set_desc(va, desc, &harness.inner.invalidator);
                    })
                    .unwrap();
            }
            let attrs = desc.mapping_attributes().unwrap();

            // Punch one page in the middle of the top block.
            let hole_idx = 300;
            punch_hole(
                root,
                VirtMemoryRegion::new(va.add_pages(hole_idx), PAGE_SIZE),
                &mut harness.create_modify_ctx(),
            )
            .unwrap();

            let leaf = read_table::<$leaf, $small>(&mut harness, small, va);
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(leaf, |ptr| {
                        let table = <$leaf>::from_ptr(ptr);
                        for idx in 0..512 {
                            let expected = if idx == hole_idx {
                                <$page_desc>::invalid()
                            } else {
                                <$page_desc>::new_mapping(pa.add_pages(idx), attrs)
                            };
                            assert_eq!(
                                table.get_idx(idx).as_raw(),
                                expected.as_raw(),
                                "page {idx} mismatch"
                            );
                        }
                    })
                    .unwrap();
            }
        }

        /// A block that genuinely *is* fully covered must still be zapped in
        /// place, without allocating a table to split it into.
        #[test]
        fn punch_whole_top_block_zaps_without_allocating() {
            let block_size: usize = 1 << 21;
            // Second-to-last block: the whole range can be expressed without
            // its exclusive end overflowing.
            let va = VA::from_value(usize::MAX - (2 * block_size) + 1);
            let pa = PA::from_value(0x8000_0000);

            let mut harness = TestHarness::new(16);
            let root = harness.inner.root_table;
            let middle = map_at_level(root, va, &mut harness.create_map_ctx()).unwrap();
            let small = map_at_level(middle, va, &mut harness.create_map_ctx()).unwrap();
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(small, |ptr| {
                        <$small>::from_ptr(ptr).set_desc(
                            va,
                            <$small_desc>::new_map_pa(pa, $mem, PtePermissions::ro(true)),
                            &harness.inner.invalidator,
                        );
                    })
                    .unwrap();
            }

            let before = harness.inner.allocator.pages_allocated;
            punch_hole(
                root,
                VirtMemoryRegion::new(va, block_size),
                &mut harness.create_modify_ctx(),
            )
            .unwrap();

            assert_eq!(
                harness.inner.allocator.pages_allocated, before,
                "a fully covered block must be zapped, not split"
            );
            unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(small, |ptr| {
                        assert_eq!(
                            <$small>::from_ptr(ptr).get_desc(va).as_raw(),
                            <$small_desc>::invalid().as_raw()
                        );
                    })
                    .unwrap();
            }
        }

        /// A region whose exclusive end would wrap past the top of the address
        /// space is rejected rather than silently truncated.
        #[test]
        fn punch_region_wrapping_address_space_is_rejected() {
            let mut harness = TestHarness::new(16);
            let root = harness.inner.root_table;
            let err = punch_hole(
                root,
                VirtMemoryRegion::new(VA::from_value(usize::MAX - PAGE_SIZE + 1), PAGE_SIZE),
                &mut harness.create_modify_ctx(),
            )
            .unwrap_err();
            assert_eq!(err, KernelError::InvalidValue);
        }
    };
}

pub(crate) use block_split_tests;

#[test]
fn descriptor_update_reports_block_and_orders_break_make() {
    use crate::arch::x86_64::memory::pg_descriptors::PDE;
    use crate::memory::{
        address::{PA, VA},
        paging::{PaMapper, PageTableEntry, TableMapper, update_descriptor},
    };
    use std::cell::Cell;

    struct RecordingInvalidator {
        slot: *const u64,
        old: u64,
        new: u64,
        phase: Cell<usize>,
    }
    impl TLBInvalidator for RecordingInvalidator {
        fn prepare(&self, change: &TranslationChange) {
            assert_eq!(self.phase.replace(1), 0);
            assert_eq!(unsafe { self.slot.read_volatile() }, self.old);
            assert_eq!(change.va.value(), 0x4000_0000);
            assert_eq!(change.size, 1 << 21);
            assert_eq!(change.old_descriptor, self.old);
            assert_eq!(change.new_descriptor, self.new);
        }
        fn invalidate(&self, _change: &TranslationChange) {
            assert_eq!(self.phase.replace(2), 1);
            assert_eq!(unsafe { self.slot.read_volatile() }, 0);
        }
        fn publish(&self, _change: &TranslationChange) {
            assert_eq!(self.phase.replace(3), 2);
            assert_eq!(unsafe { self.slot.read_volatile() }, self.new);
        }
    }
    let old = PDE::new_map_pa(
        PA::from_value(0x8000_0000),
        crate::arch::x86_64::memory::pg_descriptors::MemoryType::WB,
        super::permissions::PtePermissions::rw(false),
    );
    let new = PDE::new_next_table(TPA::from_value(0x1000));
    let mut slot = old.as_raw();
    let invalidator = RecordingInvalidator {
        slot: &slot,
        old: old.as_raw(),
        new: new.as_raw(),
        phase: Cell::new(0),
    };
    unsafe {
        update_descriptor(&mut slot, VA::from_value(0x4000_1000), new, &invalidator);
    }
    assert_eq!(invalidator.phase.get(), 3);
    // Reading via get_pte / an identity modifier must not trigger maintenance.
    unsafe {
        update_descriptor(&mut slot, VA::from_value(0x4000_1000), new, &invalidator);
    }
    assert_eq!(invalidator.phase.get(), 3);
}
