//! Page-table walking functionality

use crate::{
    error::MapError,
    memory::{
        address::{Address, GuestPhysical, GuestVirtual, MemKind, PA, Physical, TPA, VA, Virtual},
        region::VirtMemoryRegion,
    },
};

use super::{
    NullTlbInvalidator, PaMapper, PageAllocator, PageTableEntry, PageTableMapper, PgTable,
    PgTableArray, TLBInvalidator, TableMapper, TableMapperTable, permissions::PtePermissions,
};

/// A virtual address kind that can be translated through page tables.
pub trait Translatable: MemKind {
    /// The physical-side kind this translates into.
    type Phys: MemKind;
}

impl Translatable for Virtual {
    type Phys = Physical;
}

impl Translatable for GuestVirtual {
    type Phys = GuestPhysical;
}

/// A collection of context required to modify page tables.
pub struct WalkContext<'a, PM, I: TLBInvalidator> {
    /// The mapper used to temporarily access page tables by physical address.
    pub mapper: &'a mut PM,
    /// Synchronous break/make maintenance for descriptor changes.
    pub invalidator: &'a I,
}

pub(crate) fn validate_allocating_region(region: VirtMemoryRegion) -> crate::error::Result<()> {
    if !region.is_page_aligned() || !region.size().is_multiple_of(crate::memory::PAGE_SIZE) {
        return Err(MapError::VirtNotAligned.into());
    }
    if region
        .start_address()
        .value()
        .checked_add(region.size())
        .is_none()
    {
        return Err(crate::error::KernelError::InvalidValue);
    }
    Ok(())
}

pub(crate) enum WalkOperation<'a, F> {
    Modify(&'a mut F),
    PunchHole,
}

impl<F> WalkOperation<'_, F> {
    pub(crate) fn apply<D: PageTableEntry>(&mut self, va: VA, desc: D) -> D
    where
        F: FnMut(VA, D) -> D,
    {
        match self {
            Self::Modify(modifier) => modifier(va, desc),
            Self::PunchHole => D::invalid(),
        }
    }
}

pub(crate) trait RecursiveWalker<LeafDesc: PageTableEntry>: PgTable + Sized {
    fn walk<F, PM, I>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM, I>,
        modifier: &mut F,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        I: TLBInvalidator,
        F: FnMut(VA, LeafDesc) -> LeafDesc,
    {
        Self::walk_with_allocator(
            table_pa,
            region,
            ctx,
            &mut RejectBlockAllocator,
            &mut WalkOperation::Modify(modifier),
        )
    }

    fn walk_with_allocator<F, PM, A, I>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM, I>,
        allocator: &mut A,
        operation: &mut WalkOperation<F>,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        I: TLBInvalidator,
        A: PageAllocator,
        F: FnMut(VA, LeafDesc) -> LeafDesc;
}

struct RejectBlockAllocator;

impl PageAllocator for RejectBlockAllocator {
    fn allocate_page_table<T: PgTable>(&mut self) -> crate::error::Result<TPA<PgTableArray<T>>> {
        Err(MapError::NotL3Mapped.into())
    }
}

impl<T, LeafDesc: PageTableEntry> RecursiveWalker<LeafDesc> for T
where
    T: TableMapperTable,
    T::Descriptor: PaMapper,
    NextDescriptor<T>: PaMapper<Attributes = <T::Descriptor as PaMapper>::Attributes>,
    <T::Descriptor as TableMapper>::NextLevel: RecursiveWalker<LeafDesc>,
{
    fn walk_with_allocator<F, PM, A, I>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM, I>,
        allocator: &mut A,
        operation: &mut WalkOperation<F>,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        I: TLBInvalidator,
        A: PageAllocator,
        F: FnMut(VA, LeafDesc) -> LeafDesc,
    {
        walk_table_level(table_pa, region, ctx, allocator, operation, |desc| {
            let pa = desc.mapped_address().ok_or(MapError::InvalidDescriptor)?;
            let attributes = desc
                .mapping_attributes()
                .ok_or(MapError::InvalidDescriptor)?;
            Ok((pa, attributes))
        })
    }
}

type NextDescriptor<T> =
    <<<T as PgTable>::Descriptor as TableMapper>::NextLevel as PgTable>::Descriptor;

pub(crate) fn walk_table_level<T, LeafDesc, F, PM, A, S, I>(
    table_pa: TPA<PgTableArray<T>>,
    region: VirtMemoryRegion,
    ctx: &mut WalkContext<PM, I>,
    allocator: &mut A,
    operation: &mut WalkOperation<F>,
    mut mapping: S,
) -> crate::error::Result<()>
where
    T: TableMapperTable,
    NextDescriptor<T>: PaMapper,
    LeafDesc: PageTableEntry,
    <T::Descriptor as TableMapper>::NextLevel: RecursiveWalker<LeafDesc>,
    PM: PageTableMapper,
    I: TLBInvalidator,
    A: PageAllocator,
    F: FnMut(VA, LeafDesc) -> LeafDesc,
    S: FnMut(
        T::Descriptor,
    ) -> crate::error::Result<(PA, <NextDescriptor<T> as PaMapper>::Attributes)>,
{
    let table_coverage = 1 << T::Descriptor::MAP_SHIFT;

    let start_idx = T::pg_index(region.start_address());
    let end_idx = T::pg_index(region.end_address_inclusive());

    // Calculate the base address of the *entire* table.
    let table_base_va = region
        .start_address()
        .align(1 << (T::Descriptor::MAP_SHIFT + 9));

    for idx in start_idx..=end_idx {
        let entry_va = table_base_va.add_bytes(idx * table_coverage);
        let entry_va_region = VirtMemoryRegion::new(entry_va, table_coverage);

        let desc = unsafe {
            ctx.mapper
                .with_page_table(table_pa, |pgtable| T::from_ptr(pgtable).get_desc(entry_va))?
        };

        let next_desc = if let Some(next_desc) = desc.next_table_address() {
            // Point to another table; simply recurse down.
            next_desc
        } else if desc.is_valid() {
            // If descriptor is valid and doesn't point to a next table, it
            // *must* map map a PA region.
            let (pa, attributes) = mapping(desc)?;

            // See if we can simply punch the whole descriptor out here.
            if matches!(operation, WalkOperation::PunchHole) && region.contains(entry_va_region) {
                unsafe {
                    ctx.mapper.with_page_table(table_pa, |pgtable| {
                        T::from_ptr(pgtable).set_desc(
                            entry_va,
                            T::Descriptor::invalid(),
                            ctx.invalidator,
                        );
                    })?;
                }
                continue;
            }

            // We couldn't. Either we're not hole punching, or the region didn't
            // fully overlap.  Let's split, for two reasons:
            //
            //  - Arbitrary descriptor updates (WalkOperation::Modify) can only
            //  be applied to leaf entries.
            //  - If the region wasn't fully covered, for a hole punch, split to
            //  gain more granularity.
            let next_desc =
                allocator.allocate_page_table::<<T::Descriptor as TableMapper>::NextLevel>()?;
            let coverage = 1 << NextDescriptor::<T>::MAP_SHIFT;
            let entries = <T::Descriptor as TableMapper>::NextLevel::DESCRIPTORS_PER_PAGE;
            // Populate the whole replacement before making it reachable.
            unsafe {
                ctx.mapper.with_page_table(next_desc, |pgtable| {
                    let table = <T::Descriptor as TableMapper>::NextLevel::from_ptr(pgtable);
                    for idx in 0..entries {
                        let child_desc = NextDescriptor::<T>::new_mapping(
                            pa.add_bytes(idx * coverage),
                            attributes,
                        );
                        // We can use the NullTLB invalidator here as this page
                        // table isn't linked in yet.
                        table.set_desc(
                            entry_va.add_bytes(idx * coverage),
                            child_desc,
                            &NullTlbInvalidator {},
                        );
                    }
                })?;

                // Now set the new descriptor (pointing to the new table) in
                // place.
                ctx.mapper.with_page_table(table_pa, |pgtable| {
                    T::from_ptr(pgtable).set_desc(
                        entry_va,
                        T::Descriptor::new_next_table(next_desc),
                        ctx.invalidator,
                    );
                })?;
            }
            next_desc
        } else {
            // Permit sparse mappings without allocating tables for holes.
            continue;
        };

        let sub_region = entry_va_region
            .intersection(region)
            .expect("operation region should overlap with descriptor region");

        <T::Descriptor as TableMapper>::NextLevel::walk_with_allocator(
            next_desc, sub_region, ctx, allocator, operation,
        )?;
    }

    Ok(())
}

pub(crate) type TranslatorResult<P> = (Address<P, ()>, usize, PtePermissions);

pub(crate) trait Translator: PgTable + Sized {
    fn translate<M: Translatable, PM: PageTableMapper<M::Phys>, I: TLBInvalidator>(
        table_pa: Address<M::Phys, PgTableArray<Self>>,
        va: Address<M, ()>,
        ctx: &mut WalkContext<PM, I>,
    ) -> crate::error::Result<Option<TranslatorResult<M::Phys>>>;
}

impl<T> Translator for T
where
    T: TableMapperTable,
    T::Descriptor: PaMapper,
    <T::Descriptor as TableMapper>::NextLevel: Translator,
{
    fn translate<M: Translatable, PM: PageTableMapper<M::Phys>, I: TLBInvalidator>(
        table_pa: Address<M::Phys, PgTableArray<T>>,
        va: Address<M, ()>,
        ctx: &mut WalkContext<PM, I>,
    ) -> crate::error::Result<Option<(Address<M::Phys, ()>, usize, PtePermissions)>> {
        let desc = unsafe {
            ctx.mapper.with_page_table(table_pa, |pgtable| {
                // Re-tag to a normal VA here. This is safe since `get_desc()`
                // simply calculates the index into the table and returns the
                // descriptor at that point. Given we've already forced pgtable
                // to be a TVA, access to the table should be sound.
                T::from_ptr(pgtable).get_desc(VA::from_value(va.value()))
            })?
        };

        if let Some(next_pa) = desc.next_table_address() {
            // next_table_address() returns a hard-coded PA-type pointer. Recast
            // this to the `M::Phys` address-space for the next-level lookup.
            let next_pa = Address::from_value(next_pa.value());
            <T::Descriptor as TableMapper>::NextLevel::translate(next_pa, va, ctx)
        } else if let Some(block_pa) = desc.mapped_address() {
            // mapped_address() returns a hard-coded PA-type pointer. Recast
            // this to the `M::Phys` address-space for final translation result.
            let pa = Address::from_value(block_pa.value());
            let block_size = 1usize << T::Descriptor::MAP_SHIFT;
            Ok(Some((pa, block_size, desc.permissions().unwrap())))
        } else if desc.is_valid() {
            Err(MapError::InvalidDescriptor)?
        } else {
            Ok(None)
        }
    }
}
