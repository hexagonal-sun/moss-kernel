//! x86_64 page table structures, levels, and mapping logic.

use super::pg_descriptors::{MemoryType, PDE, PDPE, PML4E, PTE};
use crate::{
    error::{MapError, Result},
    memory::{
        PAGE_SIZE,
        address::{TPA, TVA, VA},
        paging::{
            PaMapper, PageAllocator, PageTableEntry, PageTableMapper, PgTable, PgTableArray,
            TLBInvalidator, TableMapper, TableMapperTable, permissions::PtePermissions,
        },
        region::{PhysMemoryRegion, VirtMemoryRegion},
    },
};

macro_rules! impl_pgtable {
    ($(#[$outer:meta])* $table:ident, desc: $desc_type:ident) => {
        #[derive(Clone, Copy)]
        $(#[$outer])*
        pub struct $table {
            base: *mut u64,
        }

        impl PgTable for $table {
            type Descriptor = $desc_type;

            fn from_ptr(ptr: TVA<PgTableArray<Self>>) -> Self {
                Self {
                    base: ptr.as_ptr_mut().cast(),
                }
            }

            fn to_raw_ptr(self) -> *mut u64 {
                self.base
            }

            fn get_idx(self, idx: usize) -> Self::Descriptor {
                debug_assert!(idx < Self::DESCRIPTORS_PER_PAGE);
                let raw = unsafe { self.base.add(idx).read_volatile() };
                Self::Descriptor::from_raw(raw)
            }

            fn get_desc(self, va: VA) -> Self::Descriptor {
                self.get_idx(Self::pg_index(va))
            }

            fn set_desc(self, va: VA, desc: Self::Descriptor, _invalidator: &dyn TLBInvalidator) {
                unsafe {
                    self.base
                        .add(Self::pg_index(va))
                        .write_volatile(PageTableEntry::as_raw(desc))
                };
            }
        }
    };
}

impl_pgtable!(
    /// PML4 page table (512 GiB per entry).
    PML4Table,
    desc: PML4E
);

impl_pgtable!(
    /// PDP page table (1 GiB per entry).
    PDPTable, desc: PDPE
);

impl_pgtable!(
    /// PDP page table (2 MiB per entry).
    PDTable, desc: PDE
);

impl_pgtable!(
    /// page table (4 kiB per entry).
    PTable, desc: PTE
);

/// Describes the attributes of a memory range to be mapped.
pub struct MapAttributes {
    /// The contiguous physical memory region to be mapped. Must be
    /// page-aligned.
    pub phys: PhysMemoryRegion,
    /// The target virtual memory region. Must be page-aligned and have the same
    /// size as `phys`.
    pub virt: VirtMemoryRegion,
    /// The architecture-specific memory attributes for the mapping.
    pub mem_type: MemoryType,
    /// The access permissions (read/write/execute, user/kernel) for the
    /// mapping.
    pub perms: PtePermissions,
}

/// A collection of context required to create page tables.
pub struct MappingContext<'a, PA, PM>
where
    PA: PageAllocator + 'a,
    PM: PageTableMapper + 'a,
{
    /// An implementation of `PageAllocator` used to request new, zeroed page
    /// tables when descending the hierarchy.
    pub allocator: &'a mut PA,
    /// An implementation of `PageTableMapper` that provides safe, temporary CPU
    /// access to page tables at their physical addresses.
    pub mapper: &'a mut PM,
    /// An object responsible for issuing TLB invalidation instructions after a
    /// mapping is successfully changed.
    pub invalidator: &'a dyn TLBInvalidator,
}

/// Maps a contiguous physical memory region to a virtual memory region.
///
/// This function walks the page table hierarchy starting from the provided root
/// (PML4) table and creates the necessary page table entries to establish the
/// mapping. It greedily attempts to use the largest possible block sizes (1GiB
/// or 2MiB) to map the region, based on the alignment and size of the remaining
/// memory ranges. If a large block mapping is not possible for a given address,
/// it descends to the next page table level, allocating new tables via the
/// allocator in the provided context as needed.
///
/// # Parameters
///
/// - `pml4_table`: The physical address of the root (PML4) page table for this
///   address space.
/// - `attrs`: A struct describing all attributes of the desired mapping, including:
///   - `phys`: The contiguous physical memory region to be mapped. Must be
///     page-aligned.
///   - `virt`: The target virtual memory region. Must be page-aligned and have
///     the same size as `phys`.
///   - `mem_type`: The memory attributes (e.g., `MemoryType::WB`,
///     `MemoryType::UC`).
///   - `perms`: The access permissions (read/write/execute, user/kernel).
/// - `ctx`: The context and services required to perform the mapping, including:
///   - `allocator`: An implementation of `PageAllocator` used to request new,
///     zeroed page tables.
///   - `mapper`: An implementation of `PageTableMapper` that provides safe,
///     temporary CPU access to page tables.
///   - `invalidator`: An object responsible for issuing TLB invalidation
///     instructions after a mapping is changed.
///
/// # Returns
///
/// Returns `Ok(())` on successful completion of the entire mapping operation.
///
/// # Errors
///
/// This function will return an error if:
///
/// - `MapError::SizeMismatch`: The sizes of the physical and virtual regions in
///   `attrs` are not equal.
/// - `MapError::TooSmall`: The size of the regions is smaller than `PAGE_SIZE`.
/// - `MapError::PhysNotAligned` or `MapError::VirtNotAligned`: The start addresses of the
///   regions in `attrs` are not page-aligned.
/// - `MapError::OutOfMemory`: The `allocator` in `ctx` fails to provide a new page table when one is needed.
/// - `MapError::AlreadyMapped`: Any part of the virtual range in `attrs` is already covered by a
///   valid page table entry.
///
/// # Panics
///
/// Panics if the logic fails to map a 4KiB page in the PTable when all
/// alignment and size checks have passed (i.e., `try_map_pa` returns `None` for
/// a PTable). This indicates a logical bug in the mapping algorithm itself.
///
/// # Safety
///
/// Although this function is not marked `unsafe`, it performs low-level
/// manipulation of memory management structures. The caller is responsible for
/// upholding several critical invariants:
///
/// 1. The `pml4_table` physical address must point to a valid, initialized PML4
///    page table.
/// 2. The `allocator` and `mapper` provided in the `ctx` must be correctly
///    implemented and functional.
/// 3. Concurrency: The caller must ensure that no other CPU core is
///    concurrently accessing or modifying the page tables being manipulated.
///    This typically requires holding a spinlock that guards the entire address
///    space.
/// 4. Memory Lifecycle: The caller is responsible for managing the lifecycle of
///    the physical memory described by `attrs.phys`. This function only creates
///    a *mapping* to the memory; it does not take ownership of it.
pub fn map_range<PA, PM>(
    pml4_table: TPA<PgTableArray<PML4Table>>,
    mut attrs: MapAttributes,
    ctx: &mut MappingContext<PA, PM>,
) -> Result<()>
where
    PA: PageAllocator,
    PM: PageTableMapper,
{
    if attrs.phys.size() != attrs.virt.size() {
        Err(MapError::SizeMismatch)?;
    }

    if attrs.phys.size() < PAGE_SIZE {
        Err(MapError::TooSmall)?;
    }

    if !attrs.phys.is_page_aligned() {
        Err(MapError::PhysNotAligned)?;
    }

    if !attrs.virt.is_page_aligned() {
        Err(MapError::VirtNotAligned)?;
    }

    while attrs.virt.size() > 0 {
        let va = attrs.virt.start_address();

        let pdp = map_at_level(pml4_table, va, ctx)?;
        if let Some(pgs_mapped) = try_map_pa(pdp, va, attrs.phys, &attrs, ctx)? {
            attrs.virt = attrs.virt.add_pages(pgs_mapped);
            attrs.phys = attrs.phys.add_pages(pgs_mapped);
            continue;
        }

        let pd = map_at_level(pdp, va, ctx)?;
        if let Some(pgs_mapped) = try_map_pa(pd, va, attrs.phys, &attrs, ctx)? {
            attrs.virt = attrs.virt.add_pages(pgs_mapped);
            attrs.phys = attrs.phys.add_pages(pgs_mapped);
            continue;
        }

        let pt = map_at_level(pd, va, ctx)?;
        try_map_pa(pt, va, attrs.phys, &attrs, ctx)?;

        attrs.virt = attrs.virt.add_pages(1);
        attrs.phys = attrs.phys.add_pages(1);
    }

    Ok(())
}

fn try_map_pa<L, PA, PM>(
    table: TPA<PgTableArray<L>>,
    va: VA,
    phys_region: PhysMemoryRegion,
    attrs: &MapAttributes,
    ctx: &mut MappingContext<PA, PM>,
) -> Result<Option<usize>>
where
    L: PgTable<Descriptor: PaMapper<MemoryType = MemoryType>>,
    PA: PageAllocator,
    PM: PageTableMapper,
{
    if L::Descriptor::could_map(phys_region, va) {
        unsafe {
            // Before creating a new mapping, check if one already exists.
            if ctx
                .mapper
                .with_page_table(table, |tbl| L::from_ptr(tbl).get_desc(va))?
                .is_valid()
            {
                // A valid descriptor (either block or table) already exists.
                return Err(MapError::AlreadyMapped)?;
            }

            // The slot is empty, so we can create the new mapping.
            ctx.mapper.with_page_table(table, |tbl| {
                L::from_ptr(tbl).set_desc(
                    va,
                    L::Descriptor::new_map_pa(
                        phys_region.start_address(),
                        attrs.mem_type,
                        attrs.perms,
                    ),
                    ctx.invalidator,
                );
            })?;
        }

        Ok(Some(1 << (L::Descriptor::MAP_SHIFT - 12)))
    } else {
        Ok(None)
    }
}

pub(super) fn map_at_level<L, PA, PM>(
    table: TPA<PgTableArray<L>>,
    va: VA,
    ctx: &mut MappingContext<PA, PM>,
) -> Result<TPA<PgTableArray<<L::Descriptor as TableMapper>::NextLevel>>>
where
    L: TableMapperTable,
    PA: PageAllocator,
    PM: PageTableMapper,
{
    unsafe {
        let desc = ctx
            .mapper
            .with_page_table(table, |pgtable| L::from_ptr(pgtable).get_desc(va))?;

        // It's already a valid table pointing to the next level. We can reuse
        // it.
        if let Some(pa) = desc.next_table_address() {
            return Ok(TPA::from_value(pa.value()));
        }

        // It's a valid descriptor, but not for a table (i.e., it's a
        // block/page). This is a conflict. We cannot replace a block mapping
        // with a table.
        if desc.is_valid() {
            return Err(MapError::AlreadyMapped)?;
        }

        // The descriptor is invalid (zero). We can create a new table.
        let new_pa = ctx
            .allocator
            .allocate_page_table::<<L::Descriptor as TableMapper>::NextLevel>()?;

        // Zero out the new table before use.
        ctx.mapper.with_page_table(new_pa, |new_pgtable| {
            core::ptr::write_bytes(new_pgtable.as_ptr_mut() as *mut _ as *mut u8, 0, PAGE_SIZE);
        })?;

        // Set the descriptor at the current level to point to the new table.
        ctx.mapper.with_page_table(table, |pgtable| {
            L::from_ptr(pgtable).set_desc(
                va,
                L::Descriptor::new_next_table(new_pa),
                ctx.invalidator,
            );
        })?;

        Ok(new_pa)
    }
}

#[cfg(test)]
#[allow(missing_docs)]
pub mod tests {
    use super::*;
    use crate::{
        arch::x86_64::memory::pg_walk::walk_and_modify_region,
        error::KernelError,
        memory::{
            address::{PA, VA},
            paging::test::{MockPageAllocator, PassthroughMapper},
        },
    };

    pub struct X86_64PagingTestHarness {
        pub inner: crate::memory::paging::test::TestHarness<PML4Table>,
    }

    impl X86_64PagingTestHarness {
        pub fn new(num_pages: usize) -> Self {
            Self {
                inner: crate::memory::paging::test::TestHarness::new(num_pages),
            }
        }

        pub fn create_map_ctx(
            &mut self,
        ) -> MappingContext<'_, MockPageAllocator, PassthroughMapper> {
            MappingContext {
                allocator: &mut self.inner.allocator,
                mapper: &mut self.inner.mapper,
                invalidator: &self.inner.invalidator,
            }
        }

        /// Helper to map a standard 4K page region
        pub fn map_4k_pages(
            &mut self,
            pa_start: usize,
            va_start: usize,
            num_pages: usize,
            perms: PtePermissions,
        ) -> Result<()> {
            let size = num_pages * PAGE_SIZE;
            map_range(
                self.inner.root_table,
                MapAttributes {
                    phys: PhysMemoryRegion::new(PA::from_value(pa_start), size),
                    virt: VirtMemoryRegion::new(VA::from_value(va_start), size),
                    mem_type: MemoryType::WB,
                    perms,
                },
                &mut self.create_map_ctx(),
            )
        }

        /// Helper to verify the permissions of a single 4K page
        pub fn verify_perms(&mut self, va: VA, expected_perms: PtePermissions) {
            let mut perms_found = None;
            walk_and_modify_region(
                self.inner.root_table,
                VirtMemoryRegion::new(va, PAGE_SIZE),
                &mut self.inner.create_walk_ctx(),
                &mut |_va, desc: PTE| {
                    perms_found = Some(desc.permissions());
                    desc // Don't modify
                },
            )
            .unwrap();
            assert_eq!(perms_found, Some(expected_perms));
        }
    }

    pub type TestHarness = X86_64PagingTestHarness;

    #[test]
    fn test_pg_index() {
        // x86_64 VA layout with 4KB pages:
        // Bits [47:39]: PML4 Index (9 bits)
        // Bits [38:30]: PDP  Index (9 bits)
        // Bits [29:21]: PD   Index (9 bits)
        // Bits [20:12]: PT   Index (9 bits)
        // Bits [11:0]:  Page Offset (12 bits)

        const PML4_IDX: u64 = 0x1A; // 26
        const PDP_IDX: u64 = 0x2B; // 43
        const PD_IDX: u64 = 0x3C; // 60
        const PT_IDX: u64 = 0x4D; // 77
        const OFFSET: u64 = 0x5E; // 94

        // Construct the virtual address from our chosen indices.
        let va_val = (PML4_IDX << 39) | (PDP_IDX << 30) | (PD_IDX << 21) | (PT_IDX << 12) | OFFSET;

        let va = VA::from_value(va_val as usize);

        // Now, test that our pg_index function can extract the original indices correctly.
        assert_eq!(PML4Table::pg_index(va), PML4_IDX as usize);
        assert_eq!(PDPTable::pg_index(va), PDP_IDX as usize);
        assert_eq!(PDTable::pg_index(va), PD_IDX as usize);
        assert_eq!(PTable::pg_index(va), PT_IDX as usize);

        const MAX_IDX: u64 = 0x1FF; // 511
        let edge_va_val = (MAX_IDX << 39) | (MAX_IDX << 30) | (MAX_IDX << 21) | (MAX_IDX << 12);
        let edge_va = VA::from_value(edge_va_val as usize);

        assert_eq!(PML4Table::pg_index(edge_va), MAX_IDX as usize);
        assert_eq!(PDPTable::pg_index(edge_va), MAX_IDX as usize);
        assert_eq!(PDTable::pg_index(edge_va), MAX_IDX as usize);
        assert_eq!(PTable::pg_index(edge_va), MAX_IDX as usize);
    }

    #[test]
    fn test_map_single_4k_page() -> Result<()> {
        let mut harness = TestHarness::new(4);

        let phys = PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE);
        let virt = VirtMemoryRegion::new(VA::from_value(0x1_0000), PAGE_SIZE);

        map_range(
            harness.inner.root_table,
            MapAttributes {
                phys,
                virt,
                mem_type: MemoryType::WB,
                perms: PtePermissions::rw(false),
            },
            &mut harness.create_map_ctx(),
        )?;

        // Verification: Walk the tables using the mapper
        let va = virt.start_address();

        // Check PML4 -> PDP
        let pdp_tpa = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(harness.inner.root_table, |pml4_tbl| {
                    PML4Table::from_ptr(pml4_tbl)
                        .next_table_pa(va)
                        .expect("PML4 table should exist")
                })?
        };

        // Check PDP -> PD
        let pd_tpa = unsafe {
            harness.inner.mapper.with_page_table(pdp_tpa, |pdp_tbl| {
                PDPTable::from_ptr(pdp_tbl)
                    .next_table_pa(va)
                    .expect("PDP table should exist")
            })?
        };

        // Check PD -> PT
        let pt_tpa = unsafe {
            harness.inner.mapper.with_page_table(pd_tpa, |pd_tbl| {
                PDTable::from_ptr(pd_tbl)
                    .next_table_pa(va)
                    .expect("PD table should exist")
            })?
        };

        // Check PT descriptor
        let pt_desc = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pt_tpa, |p_tbl| PTable::from_ptr(p_tbl).get_desc(va))?
        };

        assert!(pt_desc.is_valid());
        assert_eq!(pt_desc.permissions(), PtePermissions::rw(false));

        // Check that the mapped physical address is correct
        let raw_desc = pt_desc.as_raw();
        let mapped_pa_val = (raw_desc & !((1 << 12) - 1)) & ((1 << 48) - 1);
        assert_eq!(mapped_pa_val, phys.start_address().value() as u64);

        Ok(())
    }

    #[test]
    fn test_map_2mb_block() -> Result<()> {
        let mut harness = TestHarness::new(3);

        let block_size = 1 << 21; // 2MiB

        let phys = PhysMemoryRegion::new(PA::from_value(0x4000_0000), block_size);
        let virt = VirtMemoryRegion::new(VA::from_value(0x2000_0000), block_size);

        let attrs = MapAttributes {
            phys,
            virt,
            mem_type: MemoryType::WB,
            perms: PtePermissions::rx(true),
        };

        map_range(
            harness.inner.root_table,
            attrs,
            &mut harness.create_map_ctx(),
        )?;

        // Verification: Walk PML4 -> PDP, then check the PD descriptor
        let va = virt.start_address();

        // PML4 -> PDP
        let pdp_tpa = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(harness.inner.root_table, |tbl| {
                    PML4Table::from_ptr(tbl)
                        .next_table_pa(va)
                        .expect("PDP table should exist")
                })?
        };

        // PDP -> PD
        let pd_tpa = unsafe {
            harness.inner.mapper.with_page_table(pdp_tpa, |tbl| {
                PDPTable::from_ptr(tbl)
                    .next_table_pa(va)
                    .expect("PD table should exist")
            })?
        };

        // Check PD Desc.
        let pd_desc = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pd_tpa, |pd_tbl| PDTable::from_ptr(pd_tbl).get_desc(va))?
        };

        assert!(pd_desc.is_valid());
        assert!(
            pd_desc.next_table_address().is_none(),
            "PD entry should be a block, not a table"
        );
        assert_eq!(pd_desc.permissions(), PtePermissions::rx(true));
        assert_eq!(pd_desc.mapped_address().unwrap(), phys.start_address());

        // Only PML4, PDP and PD tables should have been allocated.
        assert_eq!(harness.inner.allocator.pages_allocated, 3);

        Ok(())
    }

    #[test]
    fn test_map_mixed_sizes() -> Result<()> {
        let mut harness = TestHarness::new(4); // PML4, PDP, PD, PT

        let block_size = 1 << 21; // 2MiB
        let total_size = block_size + PAGE_SIZE;

        let phys = PhysMemoryRegion::new(PA::from_value(0x4000_0000), total_size);
        let virt = VirtMemoryRegion::new(VA::from_value(0x2000_0000), total_size);

        let attrs = MapAttributes {
            phys,
            virt,
            mem_type: MemoryType::WB,
            perms: PtePermissions::rw(false),
        };

        map_range(
            harness.inner.root_table,
            attrs,
            &mut harness.create_map_ctx(),
        )?;

        let va1 = virt.start_address();
        let pdp_tpa = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(harness.inner.root_table, |tbl| {
                    PML4Table::from_ptr(tbl).next_table_pa(va1).unwrap()
                })?
        };

        let pd_tpa = unsafe {
            harness.inner.mapper.with_page_table(pdp_tpa, |tbl| {
                PDPTable::from_ptr(tbl).next_table_pa(va1).unwrap()
            })?
        };

        let pd_block_desc = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pd_tpa, |tbl| PDTable::from_ptr(tbl).get_desc(va1))?
        };

        assert!(pd_block_desc.next_table_address().is_none()); // it's a block.
        assert_eq!(
            pd_block_desc.mapped_address().unwrap(),
            phys.start_address()
        );
        assert!(pd_block_desc.is_valid());

        let va2 = VA::from_value(virt.start_address().value() + block_size);
        let pd_tpa = unsafe {
            harness.inner.mapper.with_page_table(pdp_tpa, |tbl| {
                PDPTable::from_ptr(tbl).next_table_pa(va2).unwrap()
            })?
        };
        let pt_tpa = unsafe {
            harness.inner.mapper.with_page_table(pd_tpa, |tbl| {
                PDTable::from_ptr(tbl).next_table_pa(va2).unwrap()
            })?
        };
        let pt_desc = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pt_tpa, |tbl| PTable::from_ptr(tbl).get_desc(va2))?
        };

        assert_eq!(
            pt_desc.mapped_address().unwrap(),
            PA::from_value(phys.start_address().value() + block_size)
        );
        assert!(pt_desc.is_valid());

        Ok(())
    }

    #[test]
    fn test_map_2mb_forced_4k_pages() -> Result<()> {
        // Force page-sized mappings by breaking alignment
        let num_pages = (1 << 21) / PAGE_SIZE; // 512 pages
        let mut harness = TestHarness::new(num_pages + 3); // PML4, PDP, PD + PTs

        let phys_base = PA::from_value(0x1000_1000); // Intentionally not 2MiB-aligned
        let virt_base = VA::from_value(0x2000_1000); // Also misaligned

        let size = 1 << 21; // 2MiB
        let phys = PhysMemoryRegion::new(phys_base, size);
        let virt = VirtMemoryRegion::new(virt_base, size);

        let attrs = MapAttributes {
            phys,
            virt,
            mem_type: MemoryType::WB,
            perms: PtePermissions::rw(true),
        };

        map_range(
            harness.inner.root_table,
            attrs,
            &mut harness.create_map_ctx(),
        )?;

        // Confirm 512 PT mappings exist
        for i in 0..num_pages {
            let va = VA::from_value(virt.start_address().value() + i * PAGE_SIZE);
            let pa = PA::from_value(phys.start_address().value() + i * PAGE_SIZE);

            let pdp_tpa = unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(harness.inner.root_table, |tbl| {
                        PML4Table::from_ptr(tbl).next_table_pa(va).unwrap()
                    })?
            };
            let pd_tpa = unsafe {
                harness.inner.mapper.with_page_table(pdp_tpa, |tbl| {
                    PDPTable::from_ptr(tbl).next_table_pa(va).unwrap()
                })?
            };
            let pt_tpa = unsafe {
                harness.inner.mapper.with_page_table(pd_tpa, |tbl| {
                    PDTable::from_ptr(tbl).next_table_pa(va).unwrap()
                })?
            };

            let desc = unsafe {
                harness
                    .inner
                    .mapper
                    .with_page_table(pt_tpa, |tbl| PTable::from_ptr(tbl).get_desc(va))?
            };

            assert!(desc.is_valid());
            assert_eq!(desc.mapped_address().unwrap(), pa);
        }

        Ok(())
    }

    #[test]
    fn test_map_out_of_memory() {
        // Only provide enough memory for PML4 and PDP tables. PD allocation should fail.
        let mut harness = TestHarness::new(2);

        let attrs = MapAttributes {
            phys: PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE),
            virt: VirtMemoryRegion::new(VA::from_value(0x1_0000), PAGE_SIZE),
            mem_type: MemoryType::WB,
            perms: PtePermissions::rw(true),
        };

        let result = map_range(
            harness.inner.root_table,
            attrs,
            &mut harness.create_map_ctx(),
        );

        assert!(matches!(result, Err(KernelError::NoMemory)));
        assert_eq!(harness.inner.allocator.pages_allocated, 2); // PML4 and PDP were allocated, failed on PD
    }

    #[test]
    fn test_map_unaligned_regions_should_fail() {
        let mut harness = TestHarness::new(1);

        // Phys region is not page-aligned
        let bad_phys = PhysMemoryRegion::new(PA::from_value(0x8003), PAGE_SIZE);
        let virt = VirtMemoryRegion::new(VA::from_value(0x1_0000), PAGE_SIZE);

        let attrs = MapAttributes {
            phys: bad_phys,
            virt,
            mem_type: MemoryType::WB,
            perms: PtePermissions::rw(true),
        };

        assert!(matches!(
            map_range(
                harness.inner.root_table,
                attrs,
                &mut harness.create_map_ctx()
            ),
            Err(KernelError::MappingError(MapError::PhysNotAligned))
        ));

        // Virt region is not page-aligned
        let phys = PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE);
        let bad_virt = VirtMemoryRegion::new(VA::from_value(0x1_003), PAGE_SIZE);

        let attrs = MapAttributes {
            phys,
            virt: bad_virt,
            mem_type: MemoryType::WB,
            perms: PtePermissions::rw(true),
        };

        assert!(matches!(
            map_range(
                harness.inner.root_table,
                attrs,
                &mut harness.create_map_ctx()
            ),
            Err(KernelError::MappingError(MapError::VirtNotAligned))
        ));
    }

    #[test]
    fn test_map_mismatched_region_sizes_should_fail() {
        let mut harness = TestHarness::new(1);

        let phys = PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE);
        let virt = VirtMemoryRegion::new(VA::from_value(0x1_0000), PAGE_SIZE * 2);

        let attrs = MapAttributes {
            phys,
            virt,
            mem_type: MemoryType::WB,
            perms: PtePermissions::rw(true),
        };

        assert!(matches!(
            map_range(
                harness.inner.root_table,
                attrs,
                &mut harness.create_map_ctx()
            ),
            Err(KernelError::MappingError(MapError::SizeMismatch))
        ));
    }

    #[test]
    fn test_remap_fails() -> Result<()> {
        let mut harness = TestHarness::new(4);

        let pa1 = PhysMemoryRegion::new(PA::from_value(0x10000), PAGE_SIZE);
        let pa2 = PhysMemoryRegion::new(PA::from_value(0x20000), PAGE_SIZE);
        let virt = VirtMemoryRegion::new(VA::from_value(0x50000), PAGE_SIZE);

        // First mapping should succeed.
        map_range(
            harness.inner.root_table,
            MapAttributes {
                phys: pa1,
                virt,
                mem_type: MemoryType::WB,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        )?;

        // Attempting to map the same VA again should fail.
        let result = map_range(
            harness.inner.root_table,
            MapAttributes {
                phys: pa2,
                virt,
                mem_type: MemoryType::WB,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        );

        assert!(matches!(
            result,
            Err(KernelError::MappingError(MapError::AlreadyMapped))
        ));

        Ok(())
    }

    #[test]
    fn test_map_page_over_block_fails() -> Result<()> {
        let mut harness = TestHarness::new(4);

        let block_size = 1 << 21; // 2MiB
        let block_va = VA::from_value(0x400_0000);

        // Map a 2MiB block.
        let phys_block = PhysMemoryRegion::new(PA::from_value(0x8000_0000), block_size);
        let virt_block = VirtMemoryRegion::new(block_va, block_size);

        map_range(
            harness.inner.root_table,
            MapAttributes {
                phys: phys_block,
                virt: virt_block,
                mem_type: MemoryType::WB,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        )?;

        // Now, attempt to map a 4k page *inside* that block's virtual address range.
        let phys_page = PhysMemoryRegion::new(PA::from_value(0x90000), PAGE_SIZE);
        let virt_page = VirtMemoryRegion::new(block_va.add_pages(1), PAGE_SIZE);

        let result = map_range(
            harness.inner.root_table,
            MapAttributes {
                phys: phys_page,
                virt: virt_page,
                mem_type: MemoryType::WB,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        );

        // This should fail in `map_at_level` when it finds the PD block descriptor
        // where it expected an invalid entry or a PT table descriptor.
        assert!(matches!(
            result,
            Err(KernelError::MappingError(MapError::AlreadyMapped))
        ));

        Ok(())
    }

    #[test]
    fn test_map_block_over_page_fails() -> Result<()> {
        let mut harness = TestHarness::new(4);

        let block_size = 1 << 21; // 2MiB
        let block_va = VA::from_value(0x400_0000);

        // Map a single 4k page at the start of where a 2MiB block would be.
        let phys_page = PhysMemoryRegion::new(PA::from_value(0x90000), PAGE_SIZE);
        let virt_page = VirtMemoryRegion::new(block_va, PAGE_SIZE);

        map_range(
            harness.inner.root_table,
            MapAttributes {
                phys: phys_page,
                virt: virt_page,
                mem_type: MemoryType::WB,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        )?;

        // Now, attempt to map a 2MiB block over the top of it.
        let phys_block = PhysMemoryRegion::new(PA::from_value(0x8000_0000), block_size);
        let virt_block = VirtMemoryRegion::new(block_va, block_size);

        let result = map_range(
            harness.inner.root_table,
            MapAttributes {
                phys: phys_block,
                virt: virt_block,
                mem_type: MemoryType::WB,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        );

        // This should fail in `try_map_pa` at the PD level, because it will
        // find a valid table descriptor (pointing to the PT table) where it
        // expected an invalid entry.
        assert!(matches!(
            result,
            Err(KernelError::MappingError(MapError::AlreadyMapped))
        ));

        Ok(())
    }

    /// Verify that table (non-leaf) entries never restrict the permissions
    /// that the leaf PTE specifies. On x86_64 the CPU ANDs R/W and U/S
    /// across all levels of the walk, so every intermediate table entry must
    /// be maximally permissive.
    #[test]
    fn test_table_entries_dont_restrict_leaf_permissions() -> Result<()> {
        let mut harness = TestHarness::new(4);

        // Map a user-accessible, read-write page.
        let phys = PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE);
        let virt = VirtMemoryRegion::new(VA::from_value(0x1_0000), PAGE_SIZE);
        let leaf_perms = PtePermissions::rw(true); // user + writable

        map_range(
            harness.inner.root_table,
            MapAttributes {
                phys,
                virt,
                mem_type: MemoryType::WB,
                perms: leaf_perms,
            },
            &mut harness.create_map_ctx(),
        )?;

        let va = virt.start_address();

        // Walk each table level and verify R/W=1, U/S=1, NX=0.
        let check_table_entry = |raw: u64, level: &str| {
            assert_ne!(raw & (1 << 1), 0, "{level} R/W bit must be set");
            assert_ne!(raw & (1 << 2), 0, "{level} U/S bit must be set");
            assert_eq!(raw & (1 << 63), 0, "{level} NX bit must be clear");
        };

        // PML4
        let pml4_raw = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(harness.inner.root_table, |tbl| {
                    PML4Table::from_ptr(tbl).get_desc(va).as_raw()
                })?
        };
        check_table_entry(pml4_raw, "PML4");

        // PDP
        let pdp_tpa = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(harness.inner.root_table, |tbl| {
                    PML4Table::from_ptr(tbl).next_table_pa(va).unwrap()
                })?
        };
        let pdp_raw = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pdp_tpa, |tbl| PDPTable::from_ptr(tbl).get_desc(va).as_raw())?
        };
        check_table_entry(pdp_raw, "PDP");

        // PD
        let pd_tpa = unsafe {
            harness.inner.mapper.with_page_table(pdp_tpa, |tbl| {
                PDPTable::from_ptr(tbl).next_table_pa(va).unwrap()
            })?
        };
        let pd_raw = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pd_tpa, |tbl| PDTable::from_ptr(tbl).get_desc(va).as_raw())?
        };
        check_table_entry(pd_raw, "PD");

        // Leaf PTE — verify it carries the actual requested permissions.
        let pt_tpa = unsafe {
            harness.inner.mapper.with_page_table(pd_tpa, |tbl| {
                PDTable::from_ptr(tbl).next_table_pa(va).unwrap()
            })?
        };
        let pt_desc = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pt_tpa, |tbl| PTable::from_ptr(tbl).get_desc(va))?
        };
        assert_eq!(pt_desc.permissions(), leaf_perms);

        Ok(())
    }

    /// Verify that a kernel-only (non-user) leaf mapping still has U/S=1 on
    /// all intermediate table entries, so other subtrees under the same tables
    /// are not locked out of user access.
    #[test]
    fn test_kernel_leaf_does_not_restrict_table_us() -> Result<()> {
        let mut harness = TestHarness::new(4);

        // Map a kernel-only, read-only page.
        let phys = PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE);
        let virt = VirtMemoryRegion::new(VA::from_value(0x1_0000), PAGE_SIZE);

        map_range(
            harness.inner.root_table,
            MapAttributes {
                phys,
                virt,
                mem_type: MemoryType::WB,
                perms: PtePermissions::ro(false), // kernel, read-only
            },
            &mut harness.create_map_ctx(),
        )?;

        let va = virt.start_address();

        // All table entries should still have U/S=1 and R/W=1 even though the
        // leaf is kernel-only and read-only.
        let pml4_raw = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(harness.inner.root_table, |tbl| {
                    PML4Table::from_ptr(tbl).get_desc(va).as_raw()
                })?
        };
        assert_ne!(pml4_raw & (1 << 1), 0, "PML4 R/W must be set");
        assert_ne!(pml4_raw & (1 << 2), 0, "PML4 U/S must be set");

        // The leaf itself should be kernel-only and read-only.
        let pdp_tpa = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(harness.inner.root_table, |tbl| {
                    PML4Table::from_ptr(tbl).next_table_pa(va).unwrap()
                })?
        };
        let pd_tpa = unsafe {
            harness.inner.mapper.with_page_table(pdp_tpa, |tbl| {
                PDPTable::from_ptr(tbl).next_table_pa(va).unwrap()
            })?
        };
        let pt_tpa = unsafe {
            harness.inner.mapper.with_page_table(pd_tpa, |tbl| {
                PDTable::from_ptr(tbl).next_table_pa(va).unwrap()
            })?
        };
        let leaf = unsafe {
            harness
                .inner
                .mapper
                .with_page_table(pt_tpa, |tbl| PTable::from_ptr(tbl).get_desc(va))?
        };
        assert_eq!(leaf.permissions(), PtePermissions::ro(false));

        Ok(())
    }
}
