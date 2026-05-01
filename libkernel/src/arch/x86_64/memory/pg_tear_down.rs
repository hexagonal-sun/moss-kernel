//! Utilities for tearing down and freeing page table hierarchies.

use super::pg_descriptors::PML4E;
use super::pg_tables::{PDPTable, PML4Table, PTable};
use crate::error::Result;
use crate::memory::paging::TableMapper;
use crate::memory::region::{PhysMemoryRegion, VirtMemoryRegion};
use crate::memory::{
    PAGE_SIZE,
    address::{TPA, VA},
    paging::{
        PaMapper, PageTableEntry, PageTableMapper, PgTable, PgTableArray,
        tear_down::{EntryKind, RecursiveTeardownWalker, TeardownAction, TeardownEntry},
        walk::WalkContext,
    },
};

// Implementation for PTable (Leaf Table)
impl RecursiveTeardownWalker for PTable {
    fn tear_down<Control, Dealloc, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        base_va: VA,
        depth: u8,
        control: &mut Control,
        deallocator: &mut Dealloc,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        Control: FnMut(&TeardownEntry) -> TeardownAction,
        Dealloc: FnMut(PhysMemoryRegion),
    {
        let mut cursor = 0;

        loop {
            let next_item = unsafe {
                ctx.mapper.with_page_table(table_pa, |pgtable| {
                    let table = PTable::from_ptr(pgtable);
                    for i in cursor..Self::DESCRIPTORS_PER_PAGE {
                        if let Some(addr) = table.get_idx(i).mapped_address() {
                            return Some((i, addr));
                        }
                    }
                    None
                })?
            };

            match next_item {
                Some((found_idx, addr)) => {
                    let entry_va =
                        base_va.add_bytes(found_idx << <PTable as PgTable>::Descriptor::MAP_SHIFT);
                    let entry = TeardownEntry {
                        kind: EntryKind::Mapping(VirtMemoryRegion::new(entry_va, PAGE_SIZE)),
                        depth,
                        region: PhysMemoryRegion::new(addr, PAGE_SIZE),
                    };
                    let action = control(&entry);

                    if !matches!(action, TeardownAction::Skip) {
                        // Clear the leaf PTE before releasing the data frame.
                        if matches!(action, TeardownAction::FreeAndClear) {
                            unsafe {
                                ctx.mapper.with_page_table(table_pa, |pgtable| {
                                    PTable::from_ptr(pgtable)
                                        .to_raw_ptr()
                                        .add(found_idx)
                                        .write_volatile(0u64);
                                })?;
                            }
                        }
                        deallocator(entry.region);
                    }

                    cursor = found_idx + 1;
                }
                None => break,
            }
        }

        Ok(())
    }
}

/// Walks the page table hierarchy for a given address space and invokes
/// `control` and `deallocator` for every frame encountered.
///
/// # Parameters
/// - `pml4_table`: Physical address of the root (PML4) page table.
/// - `ctx`: Walk context (mapper + TLB invalidator).
/// - `control`: Called for every entry *before* any recursion or deallocation.
///   Returns the [`TeardownAction`] that drives walker behaviour:
///   - [`TeardownAction::Free`] — recurse (for tables) then `deallocator`.
///   - [`TeardownAction::FreeAndClear`] — recurse, zero the PTE, then `deallocator`.
///   - [`TeardownAction::Skip`] — skip the subtree entirely.
///
///   `control` must **not** free physical memory itself.
///
///   [`EntryKind::Mapping`] entries carry their virtual address inside the
///   [`VirtMemoryRegion`] associated value. [`EntryKind::IntermediateTable`]
///   and [`EntryKind::RootTable`] entries carry no virtual-address
///   information. To filter upper-half x86_64 entries (e.g. kernel mappings),
///   inspect the VA of `Mapping` entries; note that this cannot short-circuit
///   an entire PML4 subtree, only its leaf entries.
///
/// - `deallocator`: Called by the walker (outside any live `with_page_table`
///   window) to release a frame. Always called after any PTE clearing.
///
/// Block mappings (2 MiB / 1 GiB) are reported as
/// [`EntryKind::Mapping`] with the appropriate region size.
pub fn tear_down_address_space<Control, Dealloc, PM>(
    pml4_table: TPA<PgTableArray<PML4Table>>,
    ctx: &mut WalkContext<PM>,
    mut control: Control,
    mut deallocator: Dealloc,
) -> Result<()>
where
    PM: PageTableMapper,
    Control: FnMut(&TeardownEntry) -> TeardownAction,
    Dealloc: FnMut(PhysMemoryRegion),
{
    let mut cursor = 0;

    loop {
        let next_item = unsafe {
            ctx.mapper.with_page_table(pml4_table, |pml4_tbl| {
                let table = PML4Table::from_ptr(pml4_tbl);
                for i in cursor..512 {
                    if let Some(addr) = table.get_idx(i).next_table_address() {
                        return Some((i, addr));
                    }
                }
                None
            })?
        };

        match next_item {
            Some((idx, pdp_addr)) => {
                let entry_va = VA::from_value(idx << PML4E::MAP_SHIFT);
                let entry = TeardownEntry {
                    kind: EntryKind::IntermediateTable,
                    depth: 1,
                    region: PhysMemoryRegion::new(pdp_addr.to_untyped(), PAGE_SIZE),
                };
                let action = control(&entry);

                if !matches!(action, TeardownAction::Skip) {
                    PDPTable::tear_down(
                        pdp_addr,
                        ctx,
                        entry_va,
                        1,
                        &mut control,
                        &mut deallocator,
                    )?;

                    if matches!(action, TeardownAction::FreeAndClear) {
                        unsafe {
                            ctx.mapper.with_page_table(pml4_table, |pml4_tbl| {
                                PML4Table::from_ptr(pml4_tbl)
                                    .to_raw_ptr()
                                    .add(idx)
                                    .write_volatile(0u64);
                            })?;
                        }
                    }

                    deallocator(entry.region);
                }

                cursor = idx + 1;
            }
            None => break,
        }
    }

    // Offer the root table frame to the caller.
    let root_entry = TeardownEntry {
        kind: EntryKind::RootTable,
        depth: 0,
        region: PhysMemoryRegion::new(pml4_table.to_untyped(), PAGE_SIZE),
    };
    if !matches!(control(&root_entry), TeardownAction::Skip) {
        deallocator(root_entry.region);
    }

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

    /// Tear down `root_table` freeing every frame the walker visits.
    fn capture_freed_pages<PM: PageTableMapper>(
        root_table: TPA<PgTableArray<PML4Table>>,
        ctx: &mut WalkContext<PM>,
    ) -> HashMap<usize, usize> {
        capture_freed_pages_filtered(root_table, ctx, |_| TeardownAction::Free)
    }

    /// Tear down `root_table` using a custom `control` closure.
    fn capture_freed_pages_filtered<PM, Control>(
        root_table: TPA<PgTableArray<PML4Table>>,
        ctx: &mut WalkContext<PM>,
        control: Control,
    ) -> HashMap<usize, usize>
    where
        PM: PageTableMapper,
        Control: FnMut(&TeardownEntry) -> TeardownAction,
    {
        let mut freed_map = HashMap::new();
        tear_down_address_space(root_table, ctx, control, |region| {
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
    fn teardown_skip_mappings_by_va() {
        // Demonstrates VA-based filtering of leaf mappings with the new API.
        // Unlike the old API (where `entry.va` on IntermediateTable allowed
        // skipping whole PML4 subtrees), only `Mapping` entries carry VA.
        // Intermediate table frames for the filtered subtree are still freed;
        // only the data frame itself is preserved.
        let mut harness = TestHarness::new(15);

        // Map a page in userspace (PML4 index 0).
        let user_pa = 0x1_0000;
        harness
            .map_4k_pages(user_pa, 0x0000_0001_0000_0000, 1, PtePermissions::rw(false))
            .unwrap();

        // Map a page at a "high" VA (PML4 index 256, kernel half).
        let kernel_pa = 0x2_0000;
        harness
            .map_4k_pages(
                kernel_pa,
                0xFFFF_8000_0001_0000usize,
                1,
                PtePermissions::rw(false),
            )
            .unwrap();

        // Skip Mapping entries in the upper half; free everything else.
        let freed = capture_freed_pages_filtered(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
            |entry| {
                if let EntryKind::Mapping(virt) = &entry.kind {
                    if virt.start_address().value() >= (1usize << 47) {
                        return TeardownAction::Skip;
                    }
                }
                TeardownAction::Free
            },
        );

        // The kernel data frame is preserved.
        assert!(!freed.contains_key(&kernel_pa));
        // The user data frame is freed.
        assert!(freed.contains_key(&user_pa));
        // user branch (data+PT+PD+PDP) + kernel branch (PT+PD+PDP, no data) + root = 8.
        assert_eq!(freed.len(), 8);
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

    #[test]
    fn teardown_skip_data_pages() {
        let mut harness = TestHarness::new(10);
        let va = VA::from_value(0x1_0000_0000);
        let pa = 0x8_0000;

        harness
            .map_4k_pages(pa, va.value(), 1, PtePermissions::ro(false))
            .unwrap();

        // Free only page table frames, not data pages (shared-mapping scenario).
        let freed = capture_freed_pages_filtered(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
            |entry| match entry.kind {
                EntryKind::Mapping(_) => TeardownAction::Skip,
                _ => TeardownAction::Free,
            },
        );

        // Only the 4 page table frames are freed; data page is kept.
        assert_eq!(freed.len(), 4);
        assert!(!freed.contains_key(&pa));
        assert!(freed.contains_key(&harness.inner.root_table.value()));
    }

    #[test]
    fn teardown_free_and_clear_zeroes_ptes() {
        // Map one page so each table frame has exactly one non-zero entry.
        // After FreeAndClear teardown, every table frame must have all slots zero.
        let mut harness = TestHarness::new(10);
        harness
            .map_4k_pages(0x8_0000, 0x1_0000_0000, 1, PtePermissions::ro(false))
            .unwrap();

        // Collect table frame PAs (root + all intermediates) during teardown.
        let mut table_pas: Vec<usize> = vec![harness.inner.root_table.value()];
        tear_down_address_space(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
            |entry| {
                if matches!(entry.kind, EntryKind::IntermediateTable) {
                    table_pas.push(entry.region.start_address().value());
                }
                TeardownAction::FreeAndClear
            },
            |_| {},
        )
        .unwrap();

        // Every slot in every table frame must now be zero — the parent-slot
        // clear for each level (and the leaf PTE clear) must have fired.
        // In the PassthroughMapper harness, PAs are heap pointers, so the
        // memory is still valid to read even after "freeing".
        for &table_pa in &table_pas {
            for idx in 0..512_usize {
                let slot = unsafe { (table_pa as *const u64).add(idx).read_volatile() };
                assert_eq!(
                    slot, 0,
                    "table frame {:#x} slot {idx} should be zeroed after FreeAndClear",
                    table_pa
                );
            }
        }
    }

    #[test]
    fn teardown_selective_clear() {
        // Two 4K pages in the same PT (slots 0 and 1).
        // FreeAndClear the first (VA 0x1_0000_0000), Free (no clear) the second.
        // After teardown: PT slot 0 == 0, PT slot 1 != 0.
        let mut harness = TestHarness::new(10);
        harness
            .map_4k_pages(0xA_0000, 0x1_0000_0000, 1, PtePermissions::ro(false))
            .unwrap();
        harness
            .map_4k_pages(0xB_0000, 0x1_0000_1000, 1, PtePermissions::ro(false))
            .unwrap();

        let mut pt_pa: Option<usize> = None;
        tear_down_address_space(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
            |entry| {
                // Capture the PT frame (depth-3 IntermediateTable).
                if matches!(entry.kind, EntryKind::IntermediateTable) && entry.depth == 3 {
                    pt_pa = Some(entry.region.start_address().value());
                }
                if let EntryKind::Mapping(virt) = &entry.kind {
                    if virt.start_address().value() == 0x1_0000_0000 {
                        return TeardownAction::FreeAndClear;
                    }
                    if virt.start_address().value() == 0x1_0000_1000 {
                        return TeardownAction::Free;
                    }
                }
                TeardownAction::Free
            },
            |_| {},
        )
        .unwrap();

        let pt_pa = pt_pa.expect("PT frame must have been observed");

        // The cleared slot (slot 0 in PT) must be zero.
        let slot0 = unsafe { (pt_pa as *const u64).add(0).read_volatile() };
        assert_eq!(slot0, 0, "PT slot 0 should be zeroed after FreeAndClear");

        // The un-cleared slot (slot 1 in PT) must remain non-zero.
        let slot1 = unsafe { (pt_pa as *const u64).add(1).read_volatile() };
        assert_ne!(slot1, 0, "PT slot 1 should remain non-zero after Free");
    }

    #[test]
    fn teardown_skip_root_table() {
        // RootTable + Skip must not free the root, but must still free everything else.
        let mut harness = TestHarness::new(10);
        harness
            .map_4k_pages(0x8_0000, 0x1_0000_0000, 1, PtePermissions::ro(false))
            .unwrap();

        let root_pa = harness.inner.root_table.value();
        let freed = capture_freed_pages_filtered(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
            |entry| match entry.kind {
                EntryKind::RootTable => TeardownAction::Skip,
                _ => TeardownAction::Free,
            },
        );

        assert!(
            !freed.contains_key(&root_pa),
            "Root table must not be freed"
        );
        assert!(freed.contains_key(&0x8_0000), "Data page must be freed");
        // PDP + PDE + PTE frame + data page = 4 freed (root skipped).
        assert_eq!(freed.len(), 4);
    }

    #[test]
    fn teardown_skip_entire_subtree() {
        // Map one page; Skip the depth-1 IntermediateTable (PDP frame).
        // Verify: (a) data page and all descendant frames are not freed,
        // (b) control is NOT called for descendants (no spurious recursion).
        let mut harness = TestHarness::new(10);
        let pa = 0xA_0000;
        harness
            .map_4k_pages(pa, 0x1_0000_0000, 1, PtePermissions::ro(false))
            .unwrap();

        let mut control_calls = 0usize;
        let freed = capture_freed_pages_filtered(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
            |entry| {
                control_calls += 1;
                if matches!(entry.kind, EntryKind::IntermediateTable) && entry.depth == 1 {
                    TeardownAction::Skip
                } else {
                    TeardownAction::Free
                }
            },
        );

        assert!(
            !freed.contains_key(&pa),
            "Skipped subtree data page must not be freed"
        );
        // Only the root table is freed; the rest was skipped.
        assert_eq!(freed.len(), 1);
        // control is called for root (1) + depth-1 entry (1) = 2 total.
        assert_eq!(
            control_calls, 2,
            "control must not recurse into the skipped subtree"
        );
    }

    #[test]
    fn teardown_entry_metadata() {
        // For a single 4K page at VA 0x1_0000_0000 the walker must report
        // exactly these (kind, depth, va) tuples, in any order.
        let mut harness = TestHarness::new(10);
        harness
            .map_4k_pages(0x8_0000, 0x1_0000_0000, 1, PtePermissions::ro(false))
            .unwrap();

        let mut entries: Vec<(&'static str, u8, Option<usize>)> = Vec::new();
        capture_freed_pages_filtered(
            harness.inner.root_table,
            &mut harness.inner.create_walk_ctx(),
            |e| {
                let (kind, va) = match &e.kind {
                    EntryKind::Mapping(virt) => ("Mapping", Some(virt.start_address().value())),
                    EntryKind::IntermediateTable => ("IntermediateTable", None),
                    EntryKind::RootTable => ("RootTable", None),
                };
                entries.push((kind, e.depth, va));
                TeardownAction::Free
            },
        );

        // Sort by (depth, va, kind) for a deterministic comparison.
        entries.sort_unstable_by_key(|&(k, d, v)| (d, v, k));

        assert_eq!(
            entries,
            vec![
                ("RootTable", 0, None),
                ("IntermediateTable", 1, None),      // PDP frame
                ("IntermediateTable", 2, None),      // PD frame
                ("IntermediateTable", 3, None),      // PT frame
                ("Mapping", 3, Some(0x1_0000_0000)), // mapped page
            ]
        );
    }
}
