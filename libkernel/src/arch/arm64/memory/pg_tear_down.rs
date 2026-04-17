//! Utilities for tearing down and freeing page table hierarchies.

use super::pg_tables::{L0Table, L1Table, L3Table};
use crate::error::Result;
use crate::memory::region::PhysMemoryRegion;
use crate::memory::{
    PAGE_SIZE,
    address::TPA,
    paging::{
        PaMapper, PageTableMapper, PgTable, PgTableArray, TableMapper,
        tear_down::RecursiveTeardownWalker, walk::WalkContext,
    },
};

// Implementation for L3 (Leaf Table)
impl RecursiveTeardownWalker for L3Table {
    fn tear_down<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        deallocator: &mut F,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(PhysMemoryRegion),
    {
        unsafe {
            ctx.mapper.with_page_table(table_pa, |pgtable| {
                let table = L3Table::from_ptr(pgtable);

                for idx in 0..Self::DESCRIPTORS_PER_PAGE {
                    let desc = table.get_idx(idx);

                    if let Some(addr) = desc.mapped_address() {
                        deallocator(PhysMemoryRegion::new(addr, PAGE_SIZE));
                    }
                }
            })?;
        }

        Ok(())
    }
}

/// Walks the page table hierarchy for a given address space and applies a
/// freeing closure to every allocated frame.
///
/// # Parameters
/// - `l0_table`: The physical address of the root (L0) page table.
/// - `ctx`: The context for the operation (mapper).
/// - `deallocator`: A closure called for every physical address that needs freeing,
///   along with the size of the allocation in bytes:
///     1. The User Data frames (Payload).
///     2. The L1, L2, and L3 Page Table frames.
///     3. The L0 Root Table frame.
///     
/// Note; Block mappings (2MiB / 1GiB) are freed with their actual size.
pub fn tear_down_address_space<F, PM>(
    l0_table: TPA<PgTableArray<L0Table>>,
    ctx: &mut WalkContext<PM>,
    mut deallocator: F,
) -> Result<()>
where
    PM: PageTableMapper,
    F: FnMut(PhysMemoryRegion),
{
    // L0Descriptor cannot encode block mappings, so iterate entries explicitly
    // rather than relying on the blanket RecursiveTeardownWalker impl.
    let mut cursor = 0;

    loop {
        let next_item = unsafe {
            ctx.mapper.with_page_table(l0_table, |pgtable| {
                let table = L0Table::from_ptr(pgtable);
                for i in cursor..L0Table::DESCRIPTORS_PER_PAGE {
                    if let Some(addr) = table.get_idx(i).next_table_address() {
                        return Some((i, addr));
                    }
                }
                None
            })?
        };

        match next_item {
            Some((idx, l1_addr)) => {
                L1Table::tear_down(l1_addr, ctx, &mut deallocator)?;
                deallocator(PhysMemoryRegion::new(l1_addr.to_untyped(), PAGE_SIZE));
                cursor = idx + 1;
            }
            None => break,
        }
    }

    deallocator(PhysMemoryRegion::new(l0_table.to_untyped(), PAGE_SIZE));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::arm64::memory::{
        pg_descriptors::MemoryType,
        pg_tables::{MapAttributes, map_range, tests::TestHarness},
    };
    use crate::memory::{
        PAGE_SIZE,
        address::{PA, VA},
        paging::permissions::PtePermissions,
        region::{PhysMemoryRegion, VirtMemoryRegion},
    };
    use std::collections::HashMap;

    fn capture_freed_pages<PM: PageTableMapper>(
        l0_table: TPA<PgTableArray<L0Table>>,
        ctx: &mut WalkContext<PM>,
    ) -> HashMap<usize, usize> {
        let mut freed_map = HashMap::new();
        tear_down_address_space(l0_table, ctx, |region| {
            if freed_map
                .insert(region.start_address().value(), region.size())
                .is_some()
            {
                panic!(
                    "Double free detected! Physical Address {:?} was freed twice.",
                    region
                );
            }
        })
        .expect("Teardown failed");
        freed_map
    }

    #[test]
    fn teardown_empty_table() {
        let mut harness = TestHarness::new(5);

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // Only the Root L0 table itself is freed.
        assert_eq!(freed.len(), 1);
        assert!(freed.contains_key(&harness.inner.root_table.value()));
    }

    #[test]
    fn teardown_single_page_hierarchy() {
        let mut harness = TestHarness::new(10);
        let va = VA::from_value(0x1_0000_0000);
        let pa = 0x8_0000;

        // Map a single 4k page.
        harness
            .map_4k_pages(pa, va.value(), 1, PtePermissions::ro(false))
            .unwrap();

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // 1 Payload Page (0x80000)
        // 1 L3 Table
        // 1 L2 Table
        // 1 L1 Table
        // 1 L0 Table (Root)
        assert_eq!(freed.len(), 5);
        assert!(freed.contains_key(&pa)); // The payload
        assert!(freed.contains_key(&harness.inner.root_table.value())); // The root
    }

    #[test]
    fn teardown_sparse_l3_table() {
        let mut harness = TestHarness::new(10);

        // Map index 0 of an L3 table
        let va1 = VA::from_value(0x1_0000_0000);
        let pa1 = 0xAAAA_0000;

        // Map index 511 of the SAME L3 table
        // (Assuming 4k pages, add 511 * 4096)
        let va2 = va1.add_pages(511);
        let pa2 = 0xBBBB_0000;

        harness
            .map_4k_pages(pa1, va1.value(), 1, PtePermissions::rw(false))
            .unwrap();
        harness
            .map_4k_pages(pa2, va2.value(), 1, PtePermissions::rw(false))
            .unwrap();

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // 2 Payload Pages
        // 1 L3 Table (shared)
        // 1 L2 Table
        // 1 L1 Table
        // 1 L0 Table
        assert_eq!(freed.len(), 6);
        assert!(freed.contains_key(&pa1));
        assert!(freed.contains_key(&pa2));
    }

    #[test]
    fn teardown_discontiguous_tables() {
        let mut harness = TestHarness::new(20);

        let va1 = VA::from_value(0x1_0000_0000);
        harness
            .map_4k_pages(0xA0000, va1.value(), 1, PtePermissions::rw(false))
            .unwrap();

        let va2 = VA::from_value(0x400_0000_0000);
        harness
            .map_4k_pages(0xB0000, va2.value(), 1, PtePermissions::rw(false))
            .unwrap();

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // 2 Payload Pages
        // 2 L3 Tables (one for each branch)
        // 2 L2 Tables (one for each branch)
        // 2 L1 Tables (one for each branch)
        // 1 L0 Table (Shared root)
        assert_eq!(freed.len(), 9);
    }

    #[test]
    fn teardown_full_l3_table() {
        let mut harness = TestHarness::new(10);
        let start_va = VA::from_value(0x1_0000_0000);
        let start_pa = 0x10_0000;

        // Fill an entire L3 table (512 entries)
        harness
            .map_4k_pages(start_pa, start_va.value(), 512, PtePermissions::ro(false))
            .unwrap();

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // 512 Payload Pages
        // 1 L3 Table
        // 1 L2 Table
        // 1 L1 Table
        // 1 L0 Table
        assert_eq!(freed.len(), 512 + 4);
    }

    #[test]
    fn teardown_2mb_block_mapping() {
        const BLOCK_SIZE: usize = 1 << 21; // 2MiB

        // Root + L1 + L2 = 3 allocations; no L3 needed for an L2 block.
        let mut harness = TestHarness::new(3);

        // Both PA and VA are 2MiB-aligned, so map_range will create an L2 block.
        let block_pa = 0x0020_0000usize; // 2MiB
        let block_va = 0x0020_0000usize; // 2MiB
        harness
            .map_4k_pages(block_pa, block_va, 512, PtePermissions::rw(false))
            .unwrap();

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // Block frame freed with its actual 2MiB size.
        assert_eq!(freed.get(&block_pa), Some(&BLOCK_SIZE));
        // Intermediate table frames freed with PAGE_SIZE.
        assert_eq!(
            freed.get(&harness.inner.root_table.value()),
            Some(&PAGE_SIZE)
        );
        // Total: block + L2 + L1 + L0 = 4 entries.
        assert_eq!(freed.len(), 4);
    }

    #[test]
    fn teardown_1gb_block_mapping() {
        const BLOCK_SIZE: usize = 1 << 30; // 1GiB

        // Root + L1 = 2 allocations; 1GiB block sits in an L1 descriptor.
        let mut harness = TestHarness::new(2);

        let block_pa = 0x4000_0000usize; // 1GiB-aligned
        let block_va = 0x4000_0000usize; // 1GiB-aligned
        map_range(
            harness.inner.root_table,
            MapAttributes {
                phys: PhysMemoryRegion::new(PA::from_value(block_pa), BLOCK_SIZE),
                virt: VirtMemoryRegion::new(VA::from_value(block_va), BLOCK_SIZE),
                mem_type: MemoryType::Normal,
                perms: PtePermissions::rw(false),
            },
            &mut harness.create_map_ctx(),
        )
        .unwrap();

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // Block frame freed with its actual 1GiB size.
        assert_eq!(freed.get(&block_pa), Some(&BLOCK_SIZE));
        assert_eq!(
            freed.get(&harness.inner.root_table.value()),
            Some(&PAGE_SIZE)
        );
        // Total: block + L1 + L0 = 3 entries.
        assert_eq!(freed.len(), 3);
    }
}
