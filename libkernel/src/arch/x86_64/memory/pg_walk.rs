//! Page table walking and per-entry modification.

use crate::{
    error::{MapError, Result},
    memory::{
        PAGE_SIZE,
        address::{Address, TPA, VA},
        paging::{
            NullTlbInvalidator, PaMapper, PageTableEntry, PageTableMapper, PgTable, PgTableArray,
            TableMapper,
            permissions::PtePermissions,
            walk::{RecursiveWalker, Translatable, Translator, WalkContext},
        },
        region::{MemoryRegion, VirtMemoryRegion},
    },
};

use super::{
    pg_descriptors::PTE,
    pg_tables::{PDPTable, PML4Table, PTable},
};

impl RecursiveWalker<PTE> for PTable {
    fn walk<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM>,
        modifier: &mut F,
    ) -> Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(VA, PTE) -> PTE,
    {
        unsafe {
            ctx.mapper.with_page_table(table_pa, |pgtable| {
                let table = Self::from_ptr(pgtable);
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
    pml4_table: TPA<PgTableArray<PML4Table>>,
    region: VirtMemoryRegion,
    ctx: &mut WalkContext<PM>,
    mut modifier: F, // Pass closure as a mutable ref to be used across recursive calls
) -> Result<()>
where
    PM: PageTableMapper,
    F: FnMut(VA, PTE) -> PTE,
{
    if !region.is_page_aligned() {
        Err(MapError::VirtNotAligned)?;
    }

    if region.size() == 0 {
        return Ok(()); // Nothing to do for an empty region.
    }

    PML4Table::walk(pml4_table, region, ctx, &mut modifier)
}

/// Obtain the PTE that mapps the VA into the current address space.
pub fn get_pte<PM: PageTableMapper>(
    pml4_table: TPA<PgTableArray<PML4Table>>,
    va: VA,
    mapper: &mut PM,
) -> Result<Option<PTE>> {
    let mut descriptor = None;

    let mut walk_ctx = WalkContext {
        mapper,
        // Safe to not invalidate the TLB, as we are not modifying any PTEs.
        invalidator: &NullTlbInvalidator {},
    };

    walk_and_modify_region(
        pml4_table,
        VirtMemoryRegion::new(va.page_aligned(), PAGE_SIZE),
        &mut walk_ctx,
        |_, pte| {
            descriptor = Some(pte);
            pte
        },
    )?;

    Ok(descriptor)
}

impl Translator for PML4Table {
    fn translate<M: Translatable, PM: PageTableMapper<M::Phys>>(
        table_pa: Address<M::Phys, PgTableArray<PML4Table>>,
        va: Address<M, ()>,
        ctx: &mut WalkContext<PM>,
    ) -> crate::error::Result<Option<(Address<M::Phys, ()>, usize, PtePermissions)>> {
        let desc = unsafe {
            ctx.mapper.with_page_table(table_pa, |pgtable| {
                Self::from_ptr(pgtable).get_desc(VA::from_value(va.value()))
            })?
        };
        match desc.next_table_address() {
            Some(next_pa) => {
                let next_pa = Address::from_value(next_pa.value());
                PDPTable::translate(next_pa, va, ctx)
            }
            None if desc.is_valid() => Err(MapError::InvalidDescriptor.into()),
            None => Ok(None),
        }
    }
}

impl Translator for PTable {
    fn translate<M: Translatable, PM: PageTableMapper<M::Phys>>(
        table_pa: Address<M::Phys, PgTableArray<PTable>>,
        va: Address<M, ()>,
        ctx: &mut WalkContext<PM>,
    ) -> crate::error::Result<Option<(Address<M::Phys, ()>, usize, PtePermissions)>> {
        let desc = unsafe {
            ctx.mapper.with_page_table(table_pa, |pgtable| {
                Self::from_ptr(pgtable).get_desc(VA::from_value(va.value()))
            })?
        };

        match desc.mapped_address() {
            Some(pa) => Ok(Some((
                Address::from_value(pa.value()),
                1 << Self::Descriptor::MAP_SHIFT,
                desc.permissions(),
            ))),
            None if desc.is_valid() => Err(MapError::InvalidDescriptor.into()),
            None => Ok(None),
        }
    }
}

/// Result of a translation, returning the region which is mapped (could be
/// longer than [PAGE_SIZE] for block mappings), the offset of the virtual
/// address into the region and the permissions of the mapping.
pub type TranslationResult<K> = (MemoryRegion<K>, usize, PtePermissions);

/// Translates the VA into a physical region plus an offset and permissions.
pub fn translate<M: Translatable, PM: PageTableMapper<M::Phys>>(
    pml4_table: Address<M::Phys, PgTableArray<PML4Table>>,
    va: Address<M, ()>,
    mapper: &mut PM,
) -> Result<Option<TranslationResult<M::Phys>>> {
    let mut walk_ctx = WalkContext {
        mapper,
        // Safe to not invalidate the TLB, as we are not modifying any PTEs.
        invalidator: &NullTlbInvalidator {},
    };

    if let Some((pa, blk_sz, perms)) = PML4Table::translate(pml4_table, va, &mut walk_ctx)? {
        debug_assert!(blk_sz.is_power_of_two());

        let offset = va.value() & (blk_sz - 1);

        let rgn = MemoryRegion::new(pa, blk_sz);

        Ok(Some((rgn, offset, perms)))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        arch::x86_64::memory::{
            pg_descriptors::{MemoryType, PDE},
            pg_tables::{PDPTable, PDTable, map_at_level, tests::TestHarness},
        },
        error::KernelError,
        memory::{
            PAGE_SIZE,
            address::{PA, VA},
            paging::{PaMapper, permissions::PtePermissions},
        },
    };
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
            harness.inner.root_table,
            VirtMemoryRegion::new(va, PAGE_SIZE),
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, desc: PTE| {
                modifier_was_called = true;
                // Create a new descriptor with new permissions
                PTE::new_map_pa(
                    desc.mapped_address().unwrap(),
                    MemoryType::WB,
                    PtePermissions::rw(false),
                )
            },
        )
        .unwrap();

        assert!(modifier_was_called);
        harness.verify_perms(va, PtePermissions::rw(false));
    }

    #[test]
    fn walk_contiguous_region_in_one_ptable() {
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
            harness.inner.root_table,
            region,
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, desc| {
                counter.fetch_add(1, Ordering::SeqCst);
                desc
            },
        )
        .unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), num_pages);
    }

    #[test]
    fn walk_region_spanning_multiple_ptables() {
        let mut harness = TestHarness::new(5);
        // This VA range will cross an PD entry boundary, forcing a walk over
        // two PTables. PD entries covers 2MiB. Let's map a region around a 2MiB
        // boundary.
        let pd_boundary = 1 << <PDTable as PgTable>::Descriptor::MAP_SHIFT; // 2MiB
        let va_start = VA::from_value(pd_boundary - 5 * PAGE_SIZE);
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
            harness.inner.root_table,
            region,
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, desc| {
                counter.fetch_add(1, Ordering::SeqCst);
                desc
            },
        )
        .unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), num_pages);
    }

    #[test]
    fn walk_region_spanning_pd_tables() {
        let mut harness = TestHarness::new(6);
        // This VA range will cross an PDP entry boundary, forcing a walk over
        // two PD tables.
        let pdp_boundary = 1 << <PDPTable as PgTable>::Descriptor::MAP_SHIFT; // 1GiB
        let va_start = VA::from_value(pdp_boundary - 5 * PAGE_SIZE);
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
            harness.inner.root_table,
            region,
            &mut harness.inner.create_walk_ctx(),
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
            harness.inner.root_table,
            entire_region,
            &mut harness.inner.create_walk_ctx(),
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
        let pdp =
            map_at_level(harness.inner.root_table, va, &mut harness.create_map_ctx()).unwrap();
        let pd = map_at_level(pdp, va, &mut harness.create_map_ctx()).unwrap();
        let pd_desc = PDE::new_map_pa(pa, MemoryType::WB, PtePermissions::rw(false));
        unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pd, |pd_tbl| {
                    let table = PDTable::from_ptr(pd_tbl);
                    table.set_desc(va, pd_desc, &harness.inner.invalidator);
                })
                .unwrap();
        }

        let region = VirtMemoryRegion::new(va, PAGE_SIZE);
        let result = walk_and_modify_region(
            harness.inner.root_table,
            region,
            &mut harness.inner.create_walk_ctx(),
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
            harness.inner.root_table,
            region,
            &mut harness.inner.create_walk_ctx(),
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
            harness.inner.root_table,
            region,
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, _desc| panic!("Modifier should not be called for empty region"),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn walk_unaligned_region_fails() {
        let mut harness = TestHarness::new(10);
        let region = VirtMemoryRegion::new(VA::from_value(123), PAGE_SIZE); // Not page-aligned
        let result = walk_and_modify_region(
            harness.inner.root_table,
            region,
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, desc| desc,
        );
        assert!(matches!(
            result,
            Err(KernelError::MappingError(MapError::VirtNotAligned))
        ));
    }

    #[test]
    fn walk_last_pml4_entry() {
        // The last PML4 entry (index 511) covers [0xffff_ff80_0000_0000, END).
        // Computing `entry_va + coverage` used to overflow usize for this entry.
        let mut harness = TestHarness::new(4);

        let va = VA::from_value(0xffffffff80000000);
        let pa = 0x8_0000;

        harness
            .map_4k_pages(pa, va.value(), 1, PtePermissions::ro(false))
            .unwrap();
        harness
            .map_4k_pages(
                pa + PAGE_SIZE,
                va.value() + PAGE_SIZE,
                1,
                PtePermissions::ro(false),
            )
            .unwrap();

        let mut was_called = false;
        walk_and_modify_region(
            harness.inner.root_table,
            VirtMemoryRegion::new(va.add_pages(1), PAGE_SIZE),
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, desc: PTE| {
                was_called = true;
                assert_eq!(desc.mapped_address().unwrap().value(), pa + PAGE_SIZE);
                desc
            },
        )
        .unwrap();

        assert!(was_called);
    }

    #[test]
    fn walk_last_pdpt_entry() {
        // The last PDPT entry (index 511 within PML4[511]) covers the last 1 GiB:
        // [0xffff_ffff_c000_0000, END). `entry_va + coverage` overflowed usize here.
        let mut harness = TestHarness::new(4);

        let va = VA::from_value(0xffffffff_c0000000);
        let pa = 0x9_0000;

        harness
            .map_4k_pages(pa, va.value(), 1, PtePermissions::ro(false))
            .unwrap();

        let mut was_called = false;
        walk_and_modify_region(
            harness.inner.root_table,
            VirtMemoryRegion::new(va, PAGE_SIZE),
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, desc: PTE| {
                was_called = true;
                assert_eq!(desc.mapped_address().unwrap().value(), pa);
                desc
            },
        )
        .unwrap();

        assert!(was_called);
    }

    #[test]
    fn walk_last_pd_entry() {
        // The last PD entry (index 511 within PDPT[511]/PML4[511]) covers the last 2 MiB:
        // [0xffff_ffff_ffe0_0000, END). `entry_va + coverage` overflowed usize here.
        let mut harness = TestHarness::new(4);

        let va = VA::from_value(0xffffffff_ffe00000);
        let pa = 0xa_0000;

        harness
            .map_4k_pages(pa, va.value(), 1, PtePermissions::ro(false))
            .unwrap();

        let mut was_called = false;
        walk_and_modify_region(
            harness.inner.root_table,
            VirtMemoryRegion::new(va, PAGE_SIZE),
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, desc: PTE| {
                was_called = true;
                assert_eq!(desc.mapped_address().unwrap().value(), pa);
                desc
            },
        )
        .unwrap();

        assert!(was_called);
    }

    #[test]
    fn walk_at_canonical_kernel_va() {
        let mut harness = TestHarness::new(4);

        // Canonical kernel VA: bits [63:48] = 0xFFFF (sign extension of bit 47 = 1).
        // PML4 index 256 — the first kernel entry.
        let va = VA::from_value(0xFFFF_8000_0001_0000usize);
        let pa = 0x8_0000;

        harness
            .map_4k_pages(pa, va.value(), 1, PtePermissions::ro(false))
            .unwrap();

        let mut was_called = false;
        walk_and_modify_region(
            harness.inner.root_table,
            VirtMemoryRegion::new(va, PAGE_SIZE),
            &mut harness.inner.create_walk_ctx(),
            &mut |_va, desc: PTE| {
                was_called = true;
                assert_eq!(desc.mapped_address().unwrap().value(), pa);
                desc
            },
        )
        .unwrap();

        assert!(was_called);
    }
}
