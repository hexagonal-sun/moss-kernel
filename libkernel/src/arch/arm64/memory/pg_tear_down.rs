use super::pg_descriptors::{PaMapper, TableMapper};
use super::pg_tables::L0Table;
use super::{
    pg_tables::{
        DESCRIPTORS_PER_PAGE, L3Table, PageTableMapper, PgTable, PgTableArray, TableMapperTable,
    },
    pg_walk::WalkContext,
};
use crate::error::Result;
use crate::memory::address::{PA, TPA};

trait RecursiveTeardownWalker: PgTable + Sized {
    fn tear_down<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        deallocator: &mut F,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(PA);
}

// Implementation for L0, L1, L2 (Intermediate Tables)
impl<T> RecursiveTeardownWalker for T
where
    T: TableMapperTable,
    T::NextLevel: RecursiveTeardownWalker,
{
    fn tear_down<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        deallocator: &mut F,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(PA),
    {
        let mut cursor = 0;

        loop {
            let next_item = unsafe {
                ctx.mapper.with_page_table(table_pa, |pgtable| {
                    let table = Self::from_ptr(pgtable);

                    for i in cursor..DESCRIPTORS_PER_PAGE {
                        let desc = table.get_idx(i);

                        if let Some(addr) = desc.next_table_address() {
                            return Some((i, addr));
                        }
                    }
                    None
                })?
            };

            match next_item {
                Some((found_idx, phys_addr)) => {
                    // Recurse first
                    T::NextLevel::tear_down(phys_addr.cast(), ctx, deallocator)?;
                    // Free the child table frame
                    deallocator(phys_addr.cast());

                    // Advance cursor to skip this entry next time
                    cursor = found_idx + 1;
                }
                None => {
                    // No more valid entries in this table.
                    break;
                }
            }
        }

        Ok(())
    }
}

// Implementation for L3 (Leaf Table)
impl RecursiveTeardownWalker for L3Table {
    fn tear_down<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        deallocator: &mut F,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(PA),
    {
        unsafe {
            ctx.mapper.with_page_table(table_pa, |pgtable| {
                let table = L3Table::from_ptr(pgtable);

                for idx in 0..DESCRIPTORS_PER_PAGE {
                    let desc = table.get_idx(idx);

                    if let Some(addr) = desc.mapped_address() {
                        deallocator(addr);
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
/// - `deallocator`: A closure called for every physical address that needs freeing.
///   This includes:
///     1. The User Data frames (Payload).
///     2. The L1, L2, and L3 Page Table frames.
///     3. The L0 Root Table frame.
pub fn tear_down_address_space<F, PM>(
    l0_table: TPA<PgTableArray<L0Table>>,
    ctx: &mut WalkContext<PM>,
    mut deallocator: F,
) -> Result<()>
where
    PM: PageTableMapper,
    F: FnMut(PA),
{
    L0Table::tear_down(l0_table, ctx, &mut deallocator)?;
    deallocator(l0_table.to_untyped());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::arm64::memory::pg_tables::tests::TestHarness;
    use crate::memory::address::VA;
    use crate::memory::permissions::PtePermissions;
    use std::collections::HashSet;

    fn capture_freed_pages<PM: PageTableMapper>(
        l0_table: TPA<PgTableArray<L0Table>>,
        ctx: &mut WalkContext<PM>,
    ) -> HashSet<usize> {
        let mut freed_set = HashSet::new();
        tear_down_address_space(l0_table, ctx, |pa| {
            if !freed_set.insert(pa.value()) {
                panic!(
                    "Double free detected! Physical Address {:?} was freed twice.",
                    pa
                );
            }
        })
        .expect("Teardown failed");
        freed_set
    }

    #[test]
    fn teardown_empty_table() {
        let mut harness = TestHarness::new(5);

        let freed = capture_freed_pages(harness.l0_table, &mut harness.create_walk_ctx());

        // Only the Root L0 table itself is freed.
        assert_eq!(freed.len(), 1);
        assert!(freed.contains(&harness.l0_table.value()));
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

        let freed = capture_freed_pages(harness.l0_table, &mut harness.create_walk_ctx());

        // 1 Payload Page (0x80000)
        // 1 L3 Table
        // 1 L2 Table
        // 1 L1 Table
        // 1 L0 Table (Root)
        assert_eq!(freed.len(), 5);
        assert!(freed.contains(&pa)); // The payload
        assert!(freed.contains(&harness.l0_table.value())); // The root
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

        let freed = capture_freed_pages(harness.l0_table, &mut harness.create_walk_ctx());

        // 2 Payload Pages
        // 1 L3 Table (shared)
        // 1 L2 Table
        // 1 L1 Table
        // 1 L0 Table
        assert_eq!(freed.len(), 6);
        assert!(freed.contains(&pa1));
        assert!(freed.contains(&pa2));
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

        let freed = capture_freed_pages(harness.l0_table, &mut harness.create_walk_ctx());

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

        let freed = capture_freed_pages(harness.l0_table, &mut harness.create_walk_ctx());

        // 512 Payload Pages
        // 1 L3 Table
        // 1 L2 Table
        // 1 L1 Table
        // 1 L0 Table
        assert_eq!(freed.len(), 512 + 4);
    }
}
