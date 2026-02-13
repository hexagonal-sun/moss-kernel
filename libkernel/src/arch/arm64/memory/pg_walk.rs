use super::{
    pg_descriptors::{L3Descriptor, PageTableEntry, TableMapper},
    pg_tables::{L0Table, L3Table, PageTableMapper, PgTable, PgTableArray, TableMapperTable},
    tlb::{NullTlbInvalidator, TLBInvalidator},
};
use crate::{
    error::{MapError, Result},
    memory::{
        PAGE_SIZE,
        address::{TPA, VA},
        region::VirtMemoryRegion,
    },
};

/// A collection of context required to modify page tables.
pub struct WalkContext<'a, PM>
where
    PM: PageTableMapper + 'a,
{
    pub mapper: &'a mut PM,
    pub invalidator: &'a dyn TLBInvalidator,
}

trait RecursiveWalker: PgTable + Sized {
    fn walk<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM>,
        modifier: &mut F,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(VA, L3Descriptor) -> L3Descriptor;
}

impl<T> RecursiveWalker for T
where
    T: TableMapperTable,
    T::NextLevel: RecursiveWalker,
{
    fn walk<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM>,
        modifier: &mut F,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(VA, L3Descriptor) -> L3Descriptor,
    {
        let table_coverage = 1 << T::SHIFT;

        let start_idx = Self::pg_index(region.start_address());
        let end_idx = Self::pg_index(region.end_address_inclusive());

        // Calculate the base address of the *entire* table.
        let table_base_va = region.start_address().align(1 << (T::SHIFT + 9));

        for idx in start_idx..=end_idx {
            let entry_va = table_base_va.add_bytes(idx * table_coverage);

            let desc = unsafe {
                ctx.mapper
                    .with_page_table(table_pa, |pgtable| T::from_ptr(pgtable).get_desc(entry_va))?
            };

            if let Some(next_desc) = desc.next_table_address() {
                let sub_region = VirtMemoryRegion::new(entry_va, table_coverage)
                    .intersection(region)
                    .expect("Sub region should overlap with parent region");

                T::NextLevel::walk(next_desc.cast(), sub_region, ctx, modifier)?;
            } else if desc.is_valid() {
                Err(MapError::NotL3Mapped)?;
            } else {
                // Permit sparse mappings.
                continue;
            }
        }

        Ok(())
    }
}

impl RecursiveWalker for L3Table {
    fn walk<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM>,
        modifier: &mut F,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(VA, L3Descriptor) -> L3Descriptor,
    {
        unsafe {
            ctx.mapper.with_page_table(table_pa, |pgtable| {
                let table = L3Table::from_ptr(pgtable);
                for va in region.iter_pages() {
                    let desc = table.get_desc(va);
                    if desc.is_valid() {
                        table.set_desc(va, modifier(va, desc), ctx.invalidator);
                    }
                }
            })
        }
    }
}

/// Walks the page table hierarchy for a given virtual memory region and applies
/// a modifying closure to every L3 (4KiB page) descriptor within that region.
//
/// # Parameters
/// - `l0_table`: The physical address of the root (L0) page table.
/// - `region`: The virtual memory region to modify. Must be page-aligned.
/// - `ctx`: The context for the operation, including the page table mapper
///   and TLB invalidator.
/// - `modifier`: A closure that will be called for each L3 descriptor found
///   within the `region`. It receives the virtual address of the page and a
///   mutable reference to its `L3Descriptor`.
///
/// # Returns
/// - `Ok(())` on success.
///
/// # Errors
/// - `MapError::VirtNotAligned`: The provided `region` is not page-aligned.
/// - `MapError::NotMapped`: Part of the `region` is not mapped down to the L3
///   level.
/// - `MapError::NotAnL3Mapping`: Part of the `region` is covered by a larger
///   block mapping (1GiB or 2MiB), which cannot be modified at the L3 level.
pub fn walk_and_modify_region<F, PM>(
    l0_table: TPA<PgTableArray<L0Table>>,
    region: VirtMemoryRegion,
    ctx: &mut WalkContext<PM>,
    mut modifier: F, // Pass closure as a mutable ref to be used across recursive calls
) -> Result<()>
where
    PM: PageTableMapper,
    F: FnMut(VA, L3Descriptor) -> L3Descriptor,
{
    if !region.is_page_aligned() {
        Err(MapError::VirtNotAligned)?;
    }

    if region.size() == 0 {
        return Ok(()); // Nothing to do for an empty region.
    }

    L0Table::walk(l0_table, region, ctx, &mut modifier)
}

/// Obtain the PTE that mapps the VA into the current address space.
pub fn get_pte<PM: PageTableMapper>(
    l0_table: TPA<PgTableArray<L0Table>>,
    va: VA,
    mapper: &mut PM,
) -> Result<Option<L3Descriptor>> {
    let mut descriptor = None;

    let mut walk_ctx = WalkContext {
        mapper,
        // Safe to not invalidate the TLB, as we are not modifying any PTEs.
        invalidator: &NullTlbInvalidator {},
    };

    walk_and_modify_region(
        l0_table,
        VirtMemoryRegion::new(va.page_aligned(), PAGE_SIZE),
        &mut walk_ctx,
        |_, pte| {
            descriptor = Some(pte);
            pte
        },
    )?;

    Ok(descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::arm64::memory::pg_descriptors::{L2Descriptor, MemoryType, PaMapper};
    use crate::arch::arm64::memory::pg_tables::tests::TestHarness;
    use crate::arch::arm64::memory::pg_tables::{L1Table, L2Table, map_at_level};
    use crate::error::KernelError;
    use crate::memory::PAGE_SIZE;
    use crate::memory::address::{PA, VA};
    use crate::memory::permissions::PtePermissions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn walk_modify_single_page() {
        let mut harness = TestHarness::new(10);
        let va = VA::from_value(0x1_0000_0000);
        let pa = 0x8_0000;

        // Map a single page with RO permissions
        harness
            .map_4k_pages(pa, va.value(), 1, PtePermissions::ro(false))
            .unwrap();
        harness.verify_perms(va, PtePermissions::ro(false));

        // Walk and modify permissions to RW
        let mut modifier_was_called = false;
        walk_and_modify_region(
            harness.l0_table,
            VirtMemoryRegion::new(va, PAGE_SIZE),
            &mut harness.create_walk_ctx(),
            &mut |_va, desc: L3Descriptor| {
                modifier_was_called = true;
                // Create a new descriptor with new permissions
                L3Descriptor::new_map_pa(
                    desc.mapped_address().unwrap(),
                    MemoryType::Normal,
                    PtePermissions::rw(false),
                )
            },
        )
        .unwrap();

        assert!(modifier_was_called);
        harness.verify_perms(va, PtePermissions::rw(false));
    }

    #[test]
    fn walk_contiguous_region_in_one_l3_table() {
        let mut harness = TestHarness::new(4);
        let num_pages = 10;
        let va_start = VA::from_value(0x2_0000_0000);
        let pa_start = 0x9_0000;
        let region = VirtMemoryRegion::new(va_start, num_pages * PAGE_SIZE);

        harness
            .map_4k_pages(
                pa_start,
                va_start.value(),
                num_pages,
                PtePermissions::ro(false),
            )
            .unwrap();

        // Walk and count the pages modified
        let counter = AtomicUsize::new(0);
        walk_and_modify_region(
            harness.l0_table,
            region,
            &mut harness.create_walk_ctx(),
            &mut |_va, desc| {
                counter.fetch_add(1, Ordering::SeqCst);
                desc
            },
        )
        .unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), num_pages);
    }

    #[test]
    fn walk_region_spanning_l3_tables() {
        let mut harness = TestHarness::new(5);
        // This VA range will cross an L2 entry boundary, forcing a walk over
        // two L3 tables. L2 entry covers 2MiB. Let's map a region around a 2MiB
        // boundary.
        let l2_boundary = 1 << L2Table::SHIFT; // 2MiB
        let va_start = VA::from_value(l2_boundary - 5 * PAGE_SIZE);
        let num_pages = 10;
        let region = VirtMemoryRegion::new(va_start, num_pages * PAGE_SIZE);

        harness
            .map_4k_pages(
                0x10_0000,
                va_start.value(),
                num_pages,
                PtePermissions::ro(true),
            )
            .unwrap();

        let counter = AtomicUsize::new(0);
        walk_and_modify_region(
            harness.l0_table,
            region,
            &mut harness.create_walk_ctx(),
            &mut |_va, desc| {
                counter.fetch_add(1, Ordering::SeqCst);
                desc
            },
        )
        .unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), num_pages);
    }

    #[test]
    fn walk_region_spanning_l2_tables() {
        let mut harness = TestHarness::new(6);
        // This VA range will cross an L1 entry boundary, forcing a walk over two L2 tables.
        let l1_boundary = 1 << L1Table::SHIFT; // 1GiB
        let va_start = VA::from_value(l1_boundary - 5 * PAGE_SIZE);
        let num_pages = 10;
        let region = VirtMemoryRegion::new(va_start, num_pages * PAGE_SIZE);

        harness
            .map_4k_pages(
                0x20_0000,
                va_start.value(),
                num_pages,
                PtePermissions::ro(false),
            )
            .unwrap();

        let counter = AtomicUsize::new(0);
        walk_and_modify_region(
            harness.l0_table,
            region,
            &mut harness.create_walk_ctx(),
            &mut |_va, desc| {
                counter.fetch_add(1, Ordering::SeqCst);
                desc
            },
        )
        .unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), num_pages);
    }

    #[test]
    fn walk_sparse_region() {
        let mut harness = TestHarness::new(10);
        let va1 = VA::from_value(0x3_0000_0000);
        let va2 = va1.add_pages(2);
        let va3 = va1.add_pages(4);

        // Map three pages with a "hole" in between
        harness
            .map_4k_pages(0x30000, va1.value(), 1, PtePermissions::ro(false))
            .unwrap();
        harness
            .map_4k_pages(0x40000, va2.value(), 1, PtePermissions::ro(false))
            .unwrap();
        harness
            .map_4k_pages(0x50000, va3.value(), 1, PtePermissions::ro(false))
            .unwrap();

        let counter = AtomicUsize::new(0);
        let entire_region = VirtMemoryRegion::new(va1, 5 * PAGE_SIZE);

        // Walk should succeed and only call the modifier for the valid pages
        walk_and_modify_region(
            harness.l0_table,
            entire_region,
            &mut harness.create_walk_ctx(),
            &mut |_va, desc| {
                counter.fetch_add(1, Ordering::SeqCst);
                desc
            },
        )
        .unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn walk_into_block_mapping_fails() {
        let mut harness = TestHarness::new(10);
        let va = VA::from_value(0x4_0000_0000);
        let pa = PA::from_value(0x80_0000); // 2MiB aligned

        // Manually create a 2MiB block mapping
        let l1 = map_at_level(harness.l0_table, va, &mut harness.create_map_ctx()).unwrap();
        let l2 = map_at_level(l1, va, &mut harness.create_map_ctx()).unwrap();
        let l2_desc = L2Descriptor::new_map_pa(pa, MemoryType::Normal, PtePermissions::rw(false));
        unsafe {
            harness
                .mapper
                .with_page_table(l2, |l2_tbl| {
                    let table = L2Table::from_ptr(l2_tbl);
                    table.set_desc(va, l2_desc, &harness.invalidator);
                })
                .unwrap();
        }

        let region = VirtMemoryRegion::new(va, PAGE_SIZE);
        let result = walk_and_modify_region(
            harness.l0_table,
            region,
            &mut harness.create_walk_ctx(),
            &mut |_va, desc| desc,
        );

        assert!(matches!(
            result,
            Err(crate::error::KernelError::MappingError(
                MapError::NotL3Mapped
            ))
        ));
    }

    #[test]
    fn walk_unmapped_region_does_nothing() {
        let mut harness = TestHarness::new(10);
        let region = VirtMemoryRegion::new(VA::from_value(0xDEADBEEF000), PAGE_SIZE);

        let counter = AtomicUsize::new(0);
        let result = walk_and_modify_region(
            harness.l0_table,
            region,
            &mut harness.create_walk_ctx(),
            &mut |_va, desc| {
                counter.fetch_add(1, Ordering::SeqCst);
                desc
            },
        );

        // The walk should succeed because it just finds nothing to modify.
        assert!(result.is_ok());
        // Crucially, the modifier should never have been called.
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn walk_empty_region() {
        let mut harness = TestHarness::new(10);
        let region = VirtMemoryRegion::new(VA::from_value(0x5_0000_0000), 0); // Zero size
        let result = walk_and_modify_region(
            harness.l0_table,
            region,
            &mut harness.create_walk_ctx(),
            &mut |_va, _desc| panic!("Modifier should not be called for empty region"),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn walk_unaligned_region_fails() {
        let mut harness = TestHarness::new(10);
        let region = VirtMemoryRegion::new(VA::from_value(123), PAGE_SIZE); // Not page-aligned
        let result = walk_and_modify_region(
            harness.l0_table,
            region,
            &mut harness.create_walk_ctx(),
            &mut |_va, desc| desc,
        );
        assert!(matches!(
            result,
            Err(KernelError::MappingError(MapError::VirtNotAligned))
        ));
    }
}
