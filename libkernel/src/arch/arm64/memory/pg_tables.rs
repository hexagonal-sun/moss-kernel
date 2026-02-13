use core::marker::PhantomData;

use super::{
    pg_descriptors::{
        L0Descriptor, L1Descriptor, L2Descriptor, L3Descriptor, MemoryType, PaMapper,
        PageTableEntry, TableMapper,
    },
    tlb::TLBInvalidator,
};
use crate::{
    error::{MapError, Result},
    memory::{
        PAGE_SIZE,
        address::{TPA, TVA, VA},
        permissions::PtePermissions,
        region::{PhysMemoryRegion, VirtMemoryRegion},
    },
};

pub const DESCRIPTORS_PER_PAGE: usize = PAGE_SIZE / core::mem::size_of::<u64>();
pub const LEVEL_MASK: usize = DESCRIPTORS_PER_PAGE - 1;

/// Trait representing a single level of the page table hierarchy.
///
/// Each implementor corresponds to a specific page table level (L0, L1, L2,
/// L3), characterized by its `SHIFT` value which determines the bits of the
/// virtual address used to index into the table.
///
/// # Associated Types
/// - `Descriptor`: The type representing an individual page table entry (PTE) at this level.
///
/// # Constants
/// - `SHIFT`: The bit position to shift the virtual address to obtain the index for this level.
///
/// # Provided Methods
/// - `pg_index(va: VA) -> usize`: Calculate the index into the page table for the given virtual address.
///
/// # Required Methods
/// - `get_desc(&self, va: VA) -> Self::Descriptor`: Retrieve the descriptor
///   (PTE) for the given virtual address.
/// - `get_desc_mut(&mut self, va: VA) -> &mut Self::Descriptor`: Get a mutable
///   reference to the descriptor, allowing updates.
pub trait PgTable: Clone + Copy {
    /// Bit shift used to extract the index for this page table level.
    const SHIFT: usize;

    /// The descriptor (page table entry) type for this level.
    type Descriptor: PageTableEntry;

    fn from_ptr(ptr: TVA<PgTableArray<Self>>) -> Self;

    fn to_raw_ptr(self) -> *mut u64;

    /// Compute the index into this page table from a virtual address.
    fn pg_index(va: VA) -> usize {
        (va.value() >> Self::SHIFT) & LEVEL_MASK
    }

    /// Get the descriptor for a given virtual address.
    fn get_desc(self, va: VA) -> Self::Descriptor;

    /// Get the descriptor for a given index.
    fn get_idx(self, idx: usize) -> Self::Descriptor;

    /// Set the value of the descriptor for a particular VA.
    fn set_desc(self, va: VA, desc: Self::Descriptor, invalidator: &dyn TLBInvalidator);
}

pub(super) trait TableMapperTable: PgTable<Descriptor: TableMapper> + Clone + Copy {
    type NextLevel: PgTable;

    /// Follows a descriptor to the next-level table if it's a valid table descriptor.
    ///
    /// This function is primarily used in tests to verify the integrity of the
    /// page table hierarchy after a mapping operation. It is not used in the hot
    /// path of `map_range` itself, so a `dead_code` warning is allowed.
    #[allow(dead_code)]
    fn next_table_pa(self, va: VA) -> Option<TPA<PgTableArray<Self::NextLevel>>> {
        let desc = self.get_desc(va);
        Some(TPA::from_value(desc.next_table_address()?.value()))
    }
}

#[derive(Clone)]
#[repr(C, align(4096))]
pub struct PgTableArray<K: PgTable> {
    pages: [u64; DESCRIPTORS_PER_PAGE],
    _phantom: PhantomData<K>,
}

impl<K: PgTable> PgTableArray<K> {
    pub const fn new() -> Self {
        Self {
            pages: [0; DESCRIPTORS_PER_PAGE],
            _phantom: PhantomData,
        }
    }
}

impl<K: PgTable> Default for PgTableArray<K> {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! impl_pgtable {
    ($table:ident, $shift:expr, $desc_type:ident) => {
        #[derive(Clone, Copy)]
        pub struct $table {
            base: *mut u64,
        }

        impl PgTable for $table {
            const SHIFT: usize = $shift;
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
                debug_assert!(idx < DESCRIPTORS_PER_PAGE);
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

impl_pgtable!(L0Table, 39, L0Descriptor);
impl TableMapperTable for L0Table {
    type NextLevel = L1Table;
}

impl_pgtable!(L1Table, 30, L1Descriptor);
impl TableMapperTable for L1Table {
    type NextLevel = L2Table;
}

impl_pgtable!(L2Table, 21, L2Descriptor);
impl TableMapperTable for L2Table {
    type NextLevel = L3Table;
}

impl_pgtable!(L3Table, 12, L3Descriptor);

/// Trait for temporarily mapping and modifying a page table located at a
/// physical address.
///
/// During early boot, there are multiple mechanisms for accessing page table memory:
/// - Identity mapping (idmap): active very early when VA = PA
/// - Fixmap: a small, reserved region of virtual memory used to map arbitrary
///   PAs temporarily
/// - Page-offset (linear map/logical map): when VA = PA + offset, typically
///   used after MMU init
///
/// This trait abstracts over those mechanisms by providing a unified way to
/// safely access and mutate a page table given its physical address.
///
/// # Safety
/// This function is `unsafe` because the caller must ensure:
/// - The given physical address `pa` is valid and correctly aligned for type `T`.
/// - The contents at that physical address represent a valid page table of type `T`.
pub trait PageTableMapper {
    /// Map a physical address to a usable reference of the page table, run the
    /// closure, and unmap.
    ///
    /// # Safety
    /// This function is `unsafe` because the caller must ensure:
    /// - The given physical address `pa` is valid and correctly aligned for type `T`.
    /// - The contents at that physical address represent a valid page table of type `T`.
    unsafe fn with_page_table<T: PgTable, R>(
        &mut self,
        pa: TPA<PgTableArray<T>>,
        f: impl FnOnce(TVA<PgTableArray<T>>) -> R,
    ) -> Result<R>;
}

/// Trait for allocating new page tables during address space setup.
///
/// The page table walker uses this allocator to request fresh page tables
/// when needed (e.g., when creating new levels in the page table hierarchy).
///
/// # Responsibilities
/// - Return a valid, zeroed (or otherwise ready) page table physical address wrapped in `TPA<T>`.
/// - Ensure the allocated page table meets the alignment and size requirements of type `T`.
pub trait PageAllocator {
    /// Allocate a new page table of type `T` and return its physical address.
    ///
    /// # Errors
    /// Returns an error if allocation fails (e.g., out of memory).
    fn allocate_page_table<T: PgTable>(&mut self) -> Result<TPA<PgTableArray<T>>>;
}

/// Describes the attributes of a memory range to be mapped.
pub struct MapAttributes {
    /// The contiguous physical memory region to be mapped. Must be
    /// page-aligned.
    pub phys: PhysMemoryRegion,
    /// The target virtual memory region. Must be page-aligned and have the same
    /// size as `phys`.
    pub virt: VirtMemoryRegion,
    /// The memory attributes (e.g., `MemoryType::Normal`, `MemoryType::Device`)
    /// for the mapping.
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
/// (L0) table and creates the necessary page table entries to establish the
/// mapping. It greedily attempts to use the largest possible block sizes (1GiB
/// or 2MiB) to map the region, based on the alignment and size of the remaining
/// memory ranges. If a large block mapping is not possible for a given address,
/// it descends to the next page table level, allocating new tables via the
/// allocator in the provided context as needed.
///
/// # Parameters
///
/// - `l0_table`: The physical address of the root (L0) page table for this
///   address space.
/// - `attrs`: A struct describing all attributes of the desired mapping, including:
///   - `phys`: The contiguous physical memory region to be mapped. Must be
///     page-aligned.
///   - `virt`: The target virtual memory region. Must be page-aligned and have
///     the same size as `phys`.
///   - `mem_type`: The memory attributes (e.g., `MemoryType::Normal`,
///     `MemoryType::Device`).
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
/// Panics if the logic fails to map a 4KiB page at Level 3 when all alignment
/// and size checks have passed (i.e., `try_map_pa` returns `None` for an L3
/// table). This indicates a logical bug in the mapping algorithm itself.
///
/// # Safety
///
/// Although this function is not marked `unsafe`, it performs low-level
/// manipulation of memory management structures. The caller is responsible for
/// upholding several critical invariants:
///
/// 1. The `l0_table` physical address must point to a valid, initialized L0
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
    l0_table: TPA<PgTableArray<L0Table>>,
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

        let l1 = map_at_level(l0_table, va, ctx)?;
        if let Some(pgs_mapped) = try_map_pa(l1, va, attrs.phys, &attrs, ctx)? {
            attrs.virt = attrs.virt.add_pages(pgs_mapped);
            attrs.phys = attrs.phys.add_pages(pgs_mapped);
            continue;
        }

        let l2 = map_at_level(l1, va, ctx)?;
        if let Some(pgs_mapped) = try_map_pa(l2, va, attrs.phys, &attrs, ctx)? {
            attrs.virt = attrs.virt.add_pages(pgs_mapped);
            attrs.phys = attrs.phys.add_pages(pgs_mapped);
            continue;
        }

        let l3 = map_at_level(l2, va, ctx)?;
        try_map_pa(l3, va, attrs.phys, &attrs, ctx)?;

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
    L: PgTable<Descriptor: PaMapper>,
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

        Ok(Some(1 << (L::Descriptor::map_shift() - 12)))
    } else {
        Ok(None)
    }
}

pub(super) fn map_at_level<L, PA, PM>(
    table: TPA<PgTableArray<L>>,
    va: VA,
    ctx: &mut MappingContext<PA, PM>,
) -> Result<TPA<PgTableArray<L::NextLevel>>>
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
        let new_pa = ctx.allocator.allocate_page_table::<L::NextLevel>()?;

        // Zero out the new table before use.
        ctx.mapper.with_page_table(new_pa, |new_pgtable| {
            core::ptr::write_bytes(new_pgtable.as_ptr_mut() as *mut _ as *mut u8, 0, PAGE_SIZE);
        })?;

        // Set the descriptor at the current level to point to the new table.
        ctx.mapper.with_page_table(table, |pgtable| {
            L::from_ptr(pgtable).set_desc(
                va,
                L::Descriptor::new_next_table(new_pa.to_untyped()),
                ctx.invalidator,
            );
        })?;

        Ok(new_pa)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::{
        arch::arm64::memory::pg_walk::{WalkContext, walk_and_modify_region},
        error::KernelError,
        memory::address::{IdentityTranslator, PA, VA},
    };

    /// A mock TLB invalidator that does nothing for unit testing.
    pub struct MockTLBInvalidator;
    impl TLBInvalidator for MockTLBInvalidator {}

    /// Mock page allocator that allocates on the host heap and uses a counter
    /// to simulate memory limits.
    pub struct MockPageAllocator {
        pages_allocated: usize,
        max_pages: usize,
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
        fn allocate_page_table<T: PgTable>(&mut self) -> Result<TPA<PgTableArray<T>>> {
            if self.pages_allocated >= self.max_pages {
                Err(KernelError::NoMemory)
            } else {
                self.pages_allocated += 1;
                // Allocate a page-aligned table on the host heap.
                let layout = std::alloc::Layout::new::<PgTableArray<L0Table>>();
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
        ) -> Result<R> {
            // The "physical address" in our test is the raw pointer from the heap.
            // Just cast it back and use it.
            Ok(f(pa.to_va::<IdentityTranslator>()))
        }
    }

    pub struct TestHarness {
        allocator: MockPageAllocator,
        pub mapper: PassthroughMapper,
        pub invalidator: MockTLBInvalidator,
        pub l0_table: TPA<PgTableArray<L0Table>>,
    }

    impl TestHarness {
        pub fn new(max_pages: usize) -> Self {
            let mut allocator = MockPageAllocator::new(max_pages);
            let l0_table = allocator.allocate_page_table::<L0Table>().unwrap();
            Self {
                allocator,
                mapper: PassthroughMapper,
                invalidator: MockTLBInvalidator,
                l0_table,
            }
        }

        pub fn create_map_ctx(
            &mut self,
        ) -> MappingContext<'_, MockPageAllocator, PassthroughMapper> {
            MappingContext {
                allocator: &mut self.allocator,
                mapper: &mut self.mapper,
                invalidator: &self.invalidator,
            }
        }

        pub fn create_walk_ctx(&mut self) -> WalkContext<'_, PassthroughMapper> {
            WalkContext {
                mapper: &mut self.mapper,
                invalidator: &self.invalidator,
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
                self.l0_table,
                MapAttributes {
                    phys: PhysMemoryRegion::new(PA::from_value(pa_start), size),
                    virt: VirtMemoryRegion::new(VA::from_value(va_start), size),
                    mem_type: MemoryType::Normal,
                    perms,
                },
                &mut self.create_map_ctx(),
            )
        }

        /// Helper to verify the permissions of a single 4K page
        pub fn verify_perms(&mut self, va: VA, expected_perms: PtePermissions) {
            let mut perms_found = None;
            walk_and_modify_region(
                self.l0_table,
                VirtMemoryRegion::new(va, PAGE_SIZE),
                &mut self.create_walk_ctx(),
                &mut |_va, desc: L3Descriptor| {
                    perms_found = desc.permissions();
                    desc // Don't modify
                },
            )
            .unwrap();
            assert_eq!(perms_found, Some(expected_perms));
        }
    }

    #[test]
    fn test_pg_index() {
        // AArch64 VA layout with 4KB pages:
        // Bits [47:39]: L0 Index (9 bits)
        // Bits [38:30]: L1 Index (9 bits)
        // Bits [29:21]: L2 Index (9 bits)
        // Bits [20:12]: L3 Index (9 bits)
        // Bits [11:0]:  Page Offset (12 bits)

        const L0_IDX: u64 = 0x1A; // 26
        const L1_IDX: u64 = 0x2B; // 43
        const L2_IDX: u64 = 0x3C; // 60
        const L3_IDX: u64 = 0x4D; // 77
        const OFFSET: u64 = 0x5E; // 94

        // Construct the virtual address from our chosen indices.
        let va_val = (L0_IDX << 39) | (L1_IDX << 30) | (L2_IDX << 21) | (L3_IDX << 12) | OFFSET;

        let va = VA::from_value(va_val as usize);

        // Now, test that our pg_index function can extract the original indices correctly.
        assert_eq!(L0Table::pg_index(va), L0_IDX as usize);
        assert_eq!(L1Table::pg_index(va), L1_IDX as usize);
        assert_eq!(L2Table::pg_index(va), L2_IDX as usize);
        assert_eq!(L3Table::pg_index(va), L3_IDX as usize);

        const MAX_IDX: u64 = 0x1FF; // 511
        let edge_va_val = (MAX_IDX << 39) | (MAX_IDX << 30) | (MAX_IDX << 21) | (MAX_IDX << 12);
        let edge_va = VA::from_value(edge_va_val as usize);

        assert_eq!(L0Table::pg_index(edge_va), MAX_IDX as usize);
        assert_eq!(L1Table::pg_index(edge_va), MAX_IDX as usize);
        assert_eq!(L2Table::pg_index(edge_va), MAX_IDX as usize);
        assert_eq!(L3Table::pg_index(edge_va), MAX_IDX as usize);
    }

    #[test]
    fn test_map_single_4k_page() -> Result<()> {
        let mut harness = TestHarness::new(4);

        let phys = PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE);
        let virt = VirtMemoryRegion::new(VA::from_value(0x1_0000), PAGE_SIZE);

        map_range(
            harness.l0_table,
            MapAttributes {
                phys,
                virt,
                mem_type: MemoryType::Normal,
                perms: PtePermissions::rw(false),
            },
            &mut harness.create_map_ctx(),
        )?;

        // Verification: Walk the tables using the mapper
        let va = virt.start_address();

        // 1. Check L0 -> L1
        let l1_tpa = unsafe {
            harness.mapper.with_page_table(harness.l0_table, |l0_tbl| {
                L0Table::from_ptr(l0_tbl)
                    .next_table_pa(va)
                    .expect("L1 table should exist")
            })?
        };
        // 2. Check L1 -> L2
        let l2_tpa = unsafe {
            harness.mapper.with_page_table(l1_tpa, |l1_tbl| {
                L1Table::from_ptr(l1_tbl)
                    .next_table_pa(va)
                    .expect("L2 table should exist")
            })?
        };
        // 3. Check L2 -> L3
        let l3_tpa = unsafe {
            harness.mapper.with_page_table(l2_tpa, |l2_tbl| {
                L2Table::from_ptr(l2_tbl)
                    .next_table_pa(va)
                    .expect("L3 table should exist")
            })?
        };
        // 4. Check L3 descriptor
        let l3_desc = unsafe {
            harness
                .mapper
                .with_page_table(l3_tpa, |l3_tbl| L3Table::from_ptr(l3_tbl).get_desc(va))?
        };

        assert!(l3_desc.is_valid());
        assert_eq!(l3_desc.permissions(), Some(PtePermissions::rw(false)));

        // Check that the mapped physical address is correct
        let raw_desc = l3_desc.as_raw();
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
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rx(true),
        };

        map_range(harness.l0_table, attrs, &mut harness.create_map_ctx())?;

        // Verification: Walk L0 -> L1, then check the L2 descriptor
        let va = virt.start_address();

        // L0 -> L1
        let l1_tpa = unsafe {
            harness.mapper.with_page_table(harness.l0_table, |tbl| {
                L0Table::from_ptr(tbl)
                    .next_table_pa(va)
                    .expect("L1 table should exist")
            })?
        };

        // L1 -> L2
        let l2_tpa = unsafe {
            harness.mapper.with_page_table(l1_tpa, |tbl| {
                L1Table::from_ptr(tbl)
                    .next_table_pa(va)
                    .expect("L2 table should exist")
            })?
        };

        // Check L2 Desc.
        let l2_desc = unsafe {
            harness
                .mapper
                .with_page_table(l2_tpa, |l2_tbl| L2Table::from_ptr(l2_tbl).get_desc(va))?
        };

        assert!(l2_desc.is_valid());
        assert!(
            l2_desc.next_table_address().is_none(),
            "L2 entry should be a block, not a table"
        );
        assert_eq!(l2_desc.permissions(), Some(PtePermissions::rx(true)));
        assert_eq!(l2_desc.mapped_address().unwrap(), phys.start_address());

        // Only L0, L1 and L2 tables should have been allocated.
        assert_eq!(harness.allocator.pages_allocated, 3);

        Ok(())
    }

    #[test]
    fn test_map_mixed_sizes() -> Result<()> {
        let mut harness = TestHarness::new(4); // L0, L1, L2, L3

        let block_size = 1 << 21; // 2MiB
        let total_size = block_size + PAGE_SIZE;

        let phys = PhysMemoryRegion::new(PA::from_value(0x4000_0000), total_size);
        let virt = VirtMemoryRegion::new(VA::from_value(0x2000_0000), total_size);

        let attrs = MapAttributes {
            phys,
            virt,
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(false),
        };

        map_range(harness.l0_table, attrs, &mut harness.create_map_ctx())?;

        let va1 = virt.start_address();
        let l1_tpa = unsafe {
            harness.mapper.with_page_table(harness.l0_table, |tbl| {
                L0Table::from_ptr(tbl).next_table_pa(va1).unwrap()
            })?
        };

        let l2_tpa = unsafe {
            harness.mapper.with_page_table(l1_tpa, |tbl| {
                L1Table::from_ptr(tbl).next_table_pa(va1).unwrap()
            })?
        };

        let l2_block_desc = unsafe {
            harness
                .mapper
                .with_page_table(l2_tpa, |tbl| L2Table::from_ptr(tbl).get_desc(va1))?
        };

        assert!(l2_block_desc.next_table_address().is_none()); // it's a block.
        assert_eq!(
            l2_block_desc.mapped_address().unwrap(),
            phys.start_address()
        );
        assert!(l2_block_desc.permissions().is_some());

        let va2 = VA::from_value(virt.start_address().value() + block_size);
        let l2_tpa = unsafe {
            harness.mapper.with_page_table(l1_tpa, |tbl| {
                L1Table::from_ptr(tbl).next_table_pa(va2).unwrap()
            })?
        };
        let l3_tpa = unsafe {
            harness.mapper.with_page_table(l2_tpa, |tbl| {
                L2Table::from_ptr(tbl).next_table_pa(va2).unwrap()
            })?
        };
        let l3_desc = unsafe {
            harness
                .mapper
                .with_page_table(l3_tpa, |tbl| L3Table::from_ptr(tbl).get_desc(va2))?
        };

        assert_eq!(
            l3_desc.mapped_address().unwrap(),
            PA::from_value(phys.start_address().value() + block_size)
        );
        assert!(l3_desc.permissions().is_some());

        Ok(())
    }

    #[test]
    fn test_map_2mb_forced_4k_pages() -> Result<()> {
        // Force page-sized mappings by breaking alignment
        let num_pages = (1 << 21) / PAGE_SIZE; // 512 pages
        let mut harness = TestHarness::new(num_pages + 3); // L0, L1, L2 + 512 L3s

        let phys_base = PA::from_value(0x1000_1000); // Intentionally not 2MiB-aligned
        let virt_base = VA::from_value(0x2000_1000); // Also misaligned

        let size = 1 << 21; // 2MiB
        let phys = PhysMemoryRegion::new(phys_base, size);
        let virt = VirtMemoryRegion::new(virt_base, size);

        let attrs = MapAttributes {
            phys,
            virt,
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(true),
        };

        map_range(harness.l0_table, attrs, &mut harness.create_map_ctx())?;

        // Confirm 512 L3 mappings exist
        for i in 0..num_pages {
            let va = VA::from_value(virt.start_address().value() + i * PAGE_SIZE);
            let pa = PA::from_value(phys.start_address().value() + i * PAGE_SIZE);

            let l1_tpa = unsafe {
                harness.mapper.with_page_table(harness.l0_table, |tbl| {
                    L0Table::from_ptr(tbl).next_table_pa(va).unwrap()
                })?
            };
            let l2_tpa = unsafe {
                harness.mapper.with_page_table(l1_tpa, |tbl| {
                    L1Table::from_ptr(tbl).next_table_pa(va).unwrap()
                })?
            };
            let l3_tpa = unsafe {
                harness.mapper.with_page_table(l2_tpa, |tbl| {
                    L2Table::from_ptr(tbl).next_table_pa(va).unwrap()
                })?
            };

            let desc = unsafe {
                harness
                    .mapper
                    .with_page_table(l3_tpa, |tbl| L3Table::from_ptr(tbl).get_desc(va))?
            };

            assert!(desc.is_valid());
            assert_eq!(desc.mapped_address().unwrap(), pa);
        }

        Ok(())
    }

    #[test]
    fn test_map_out_of_memory() {
        // Only provide enough memory for L0 and L1 tables. L2 allocation for a 4k page should fail.
        let mut harness = TestHarness::new(2);

        let attrs = MapAttributes {
            phys: PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE),
            virt: VirtMemoryRegion::new(VA::from_value(0x1_0000), PAGE_SIZE),
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(true),
        };

        let result = map_range(harness.l0_table, attrs, &mut harness.create_map_ctx());

        assert!(matches!(result, Err(KernelError::NoMemory)));
        assert_eq!(harness.allocator.pages_allocated, 2); // L0 and L1 were allocated, failed on L2
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
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(true),
        };

        assert!(matches!(
            map_range(harness.l0_table, attrs, &mut harness.create_map_ctx()),
            Err(KernelError::MappingError(MapError::PhysNotAligned))
        ));

        // Virt region is not page-aligned
        let phys = PhysMemoryRegion::new(PA::from_value(0x8_0000), PAGE_SIZE);
        let bad_virt = VirtMemoryRegion::new(VA::from_value(0x1_003), PAGE_SIZE);

        let attrs = MapAttributes {
            phys,
            virt: bad_virt,
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(true),
        };

        assert!(matches!(
            map_range(harness.l0_table, attrs, &mut harness.create_map_ctx()),
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
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(true),
        };

        assert!(matches!(
            map_range(harness.l0_table, attrs, &mut harness.create_map_ctx()),
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
            harness.l0_table,
            MapAttributes {
                phys: pa1,
                virt,
                mem_type: MemoryType::Normal,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        )?;

        // Attempting to map the same VA again should fail.
        let result = map_range(
            harness.l0_table,
            MapAttributes {
                phys: pa2,
                virt,
                mem_type: MemoryType::Normal,
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
            harness.l0_table,
            MapAttributes {
                phys: phys_block,
                virt: virt_block,
                mem_type: MemoryType::Normal,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        )?;

        // Now, attempt to map a 4k page *inside* that block's virtual address range.
        let phys_page = PhysMemoryRegion::new(PA::from_value(0x90000), PAGE_SIZE);
        let virt_page = VirtMemoryRegion::new(block_va.add_pages(1), PAGE_SIZE);

        let result = map_range(
            harness.l0_table,
            MapAttributes {
                phys: phys_page,
                virt: virt_page,
                mem_type: MemoryType::Normal,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        );

        // This should fail in `map_at_level` when it finds the L2 block descriptor
        // where it expected an invalid entry or an L3 table descriptor.
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
            harness.l0_table,
            MapAttributes {
                phys: phys_page,
                virt: virt_page,
                mem_type: MemoryType::Normal,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        )?;

        // Now, attempt to map a 2MiB block over the top of it.
        let phys_block = PhysMemoryRegion::new(PA::from_value(0x8000_0000), block_size);
        let virt_block = VirtMemoryRegion::new(block_va, block_size);

        let result = map_range(
            harness.l0_table,
            MapAttributes {
                phys: phys_block,
                virt: virt_block,
                mem_type: MemoryType::Normal,
                perms: PtePermissions::rw(true),
            },
            &mut harness.create_map_ctx(),
        );

        // This should fail in `try_map_pa` at the L2 level, because it will
        // find a valid table descriptor (pointing to the L3 table) where it
        // expected an invalid entry.
        assert!(matches!(
            result,
            Err(KernelError::MappingError(MapError::AlreadyMapped))
        ));

        Ok(())
    }
}
