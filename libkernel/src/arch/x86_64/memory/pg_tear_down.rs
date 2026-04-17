//! Utilities for tearing down and freeing page table hierarchies.

use super::pg_tables::{PDPTable, PML4Table, PTable};
use crate::error::Result;
use crate::memory::paging::TableMapper;
use crate::memory::region::PhysMemoryRegion;
use crate::memory::{
    PAGE_SIZE,
    address::TPA,
    paging::{
        PaMapper, PageTableMapper, PgTable, PgTableArray, tear_down::RecursiveTeardownWalker,
        walk::WalkContext,
    },
};

// Implementation for PTable (Leaf Table)
impl RecursiveTeardownWalker for PTable {
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
                let table = PTable::from_ptr(pgtable);

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
/// freeing closure to every lower-half canonical address allocated frame.
///
/// # Parameters
/// - `pml4_table`: The physical address of the root (PML4) page table.
/// - `ctx`: The context for the operation (mapper).
/// - `deallocator`: A closure called for every physical address that needs freeing.
///   This includes:
///     1. The User Data frames (Payload).
///     2. The PDP, PD, and PTable Page Table frames.
///     3. The PML4 Root Table frame.
///     
/// Block mappings (2MiB / 1GiB) are freed with their actual size.
///
/// *Note* PDPTables which are pointed to by PML4 indexes [256-511] are not
/// free'd.
pub fn tear_down_address_space<F, PM>(
    pml4_table: TPA<PgTableArray<PML4Table>>,
    ctx: &mut WalkContext<PM>,
    mut deallocator: F,
) -> Result<()>
where
    PM: PageTableMapper,
    F: FnMut(PhysMemoryRegion),
{
    let mut cursor = 0;

    loop {
        let next_item = unsafe {
            ctx.mapper.with_page_table(pml4_table, |pml4_tbl| {
                let table = PML4Table::from_ptr(pml4_tbl);
                for i in cursor..256 {
                    if let Some(addr) = table.get_idx(i).next_table_address() {
                        return Some((i, addr));
                    }
                }
                None
            })?
        };

        match next_item {
            Some((idx, pdp_addr)) => {
                PDPTable::tear_down(pdp_addr, ctx, &mut deallocator)?;
                deallocator(PhysMemoryRegion::new(pdp_addr.to_untyped(), PAGE_SIZE));
                cursor = idx + 1;
            }
            None => break,
        }
    }

    deallocator(PhysMemoryRegion::new(pml4_table.to_untyped(), PAGE_SIZE));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::x86_64::memory::{
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
        root_table: TPA<PgTableArray<PML4Table>>,
        ctx: &mut WalkContext<PM>,
    ) -> HashMap<usize, usize> {
        let mut freed_map = HashMap::new();
        tear_down_address_space(root_table, ctx, |region| {
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
        // 1 PTable
        // 1 PD Table
        // 1 PDP Table
        // 1 PML4 Table (Root)
        assert_eq!(freed.len(), 5);
        assert!(freed.contains_key(&pa)); // The payload
        assert!(freed.contains_key(&harness.inner.root_table.value())); // The root
    }

    #[test]
    fn teardown_sparse_ptable() {
        let mut harness = TestHarness::new(10);

        // Map index 0 of an PTable
        let va1 = VA::from_value(0x1_0000_0000);
        let pa1 = 0xAAAA_0000;

        // Map index 511 of the SAME PTable
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
        // 1 PML4 Table (shared)
        // 1 PD Table
        // 1 PDP Table
        // 1 PML4 Table
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
        // 2 PTable (one for each branch)
        // 2 PD Tables (one for each branch)
        // 2 PDP Tables (one for each branch)
        // 1 PML4 Table (Shared root)
        assert_eq!(freed.len(), 9);
    }

    #[test]
    fn teardown_full_ptable() {
        let mut harness = TestHarness::new(10);
        let start_va = VA::from_value(0x1_0000_0000);
        let start_pa = 0x10_0000;

        // Fill an entire PTable table (512 entries)
        harness
            .map_4k_pages(start_pa, start_va.value(), 512, PtePermissions::ro(false))
            .unwrap();

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // 512 Payload Pages
        // 1 PTable
        // 1 PD Table
        // 1 PDP Table
        // 1 PML4 Table
        assert_eq!(freed.len(), 512 + 4);
    }

    #[test]
    fn teardown_does_not_free_kernel_tables() {
        let mut harness = TestHarness::new(15);

        // Map a page in userspace (PML4 index 0).
        let user_pa = 0x1_0000;
        harness
            .map_4k_pages(user_pa, 0x0000_0001_0000_0000, 1, PtePermissions::rw(false))
            .unwrap();

        // Map a page at a canonical kernel VA (PML4 index 256, bits [63:48] = 0xFFFF).
        // This populates the upper half of the PML4, which tear_down_address_space
        // must NOT touch.
        let kernel_pa = 0x2_0000;
        harness
            .map_4k_pages(
                kernel_pa,
                0xFFFF_8000_0001_0000usize,
                1,
                PtePermissions::rw(false),
            )
            .unwrap();

        let freed = capture_freed_pages(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
        );

        // Only the userspace hierarchy must be freed:
        //   1 payload (user_pa)
        //   1 PT table  (userspace branch)
        //   1 PD table  (userspace branch)
        //   1 PDP table (userspace branch)
        //   1 PML4 root
        assert_eq!(freed.len(), 5);
        assert!(freed.contains_key(&user_pa));

        // The kernel payload and its intermediate tables (PML4 index >= 256) must
        // not be freed — they belong to the kernel and outlive this address space.
        assert!(!freed.contains_key(&kernel_pa));
    }

    #[test]
    fn teardown_2mb_block_mapping() {
        const BLOCK_SIZE: usize = 1 << 21; // 2MiB

        // Root + PDP + PD = 3 allocations; no PT needed for a block mapping.
        let mut harness = TestHarness::new(3);

        // Both PA and VA are 2MiB-aligned, so map_range will create a PDE block.
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
        // Total: block + PD + PDP + PML4 = 4 entries.
        assert_eq!(freed.len(), 4);
    }

    #[test]
    fn teardown_1gb_block_mapping() {
        const BLOCK_SIZE: usize = 1 << 30; // 1GiB

        // Root + PDP = 2 allocations; 1GiB block sits in a PDPE, no PD or PT needed.
        let mut harness = TestHarness::new(2);

        let block_pa = 0x4000_0000usize; // 1GiB-aligned
        let block_va = 0x4000_0000usize; // 1GiB-aligned
        map_range(
            harness.inner.root_table,
            MapAttributes {
                phys: PhysMemoryRegion::new(PA::from_value(block_pa), BLOCK_SIZE),
                virt: VirtMemoryRegion::new(VA::from_value(block_va), BLOCK_SIZE),
                mem_type: MemoryType::WB,
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
        // Total: block + PDP + PML4 = 3 entries.
        assert_eq!(freed.len(), 3);
    }
}
