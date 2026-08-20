//! Page-table walking functionality

use crate::{
    error::MapError,
    memory::{
        address::{Address, GuestPhysical, GuestVirtual, MemKind, Physical, TPA, VA, Virtual},
        region::VirtMemoryRegion,
    },
};

use super::{
    PaMapper, PageTableEntry, PageTableMapper, PgTable, PgTableArray, TLBInvalidator, TableMapper,
    TableMapperTable, permissions::PtePermissions,
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
pub struct WalkContext<'a, PM> {
    /// The mapper used to temporarily access page tables by physical address.
    pub mapper: &'a mut PM,
    /// The TLB invalidator invoked after modifying page table entries.
    pub invalidator: &'a dyn TLBInvalidator,
}

pub(crate) trait RecursiveWalker<LeafDesc: PageTableEntry>: PgTable + Sized {
    fn walk<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM>,
        modifier: &mut F,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(VA, LeafDesc) -> LeafDesc;
}

impl<T, LeafDesc: PageTableEntry> RecursiveWalker<LeafDesc> for T
where
    T: TableMapperTable,
    <T::Descriptor as TableMapper>::NextLevel: RecursiveWalker<LeafDesc>,
{
    fn walk<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        region: VirtMemoryRegion,
        ctx: &mut WalkContext<PM>,
        modifier: &mut F,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(VA, LeafDesc) -> LeafDesc,
    {
        let table_coverage = 1 << T::Descriptor::MAP_SHIFT;

        let start_idx = Self::pg_index(region.start_address());
        let end_idx = Self::pg_index(region.end_address_inclusive());

        // Calculate the base address of the *entire* table.
        let table_base_va = region
            .start_address()
            .align(1 << (T::Descriptor::MAP_SHIFT + 9));

        for idx in start_idx..=end_idx {
            let entry_va = table_base_va.add_bytes(idx * table_coverage);

            let desc = unsafe {
                ctx.mapper
                    .with_page_table(table_pa, |pgtable| T::from_ptr(pgtable).get_desc(entry_va))?
            };

            if let Some(next_desc) = desc.next_table_address() {
                // `entry_va + table_coverage` can overflow for the last entry
                // at each level (e.g. PML4 entry 511 on x86_64). Compute the
                // intersection with saturating arithmetic instead of
                // constructing an unrepresentable VirtMemoryRegion.
                let entry_end = entry_va.value().saturating_add(table_coverage);
                let sub_start = region.start_address().value().max(entry_va.value());
                let sub_end = region.end_address().value().min(entry_end);
                let sub_region = VirtMemoryRegion::from_start_end_address(
                    VA::from_value(sub_start),
                    VA::from_value(sub_end),
                );

                <T::Descriptor as TableMapper>::NextLevel::walk(
                    next_desc, sub_region, ctx, modifier,
                )?;
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

pub(crate) type TranslatorResult<P> = (Address<P, ()>, usize, PtePermissions);

pub(crate) trait Translator: PgTable + Sized {
    fn translate<M: Translatable, PM: PageTableMapper<M::Phys>>(
        table_pa: Address<M::Phys, PgTableArray<Self>>,
        va: Address<M, ()>,
        ctx: &mut WalkContext<PM>,
    ) -> crate::error::Result<Option<TranslatorResult<M::Phys>>>;
}

impl<T> Translator for T
where
    T: TableMapperTable,
    T::Descriptor: PaMapper,
    <T::Descriptor as TableMapper>::NextLevel: Translator,
{
    fn translate<M: Translatable, PM: PageTableMapper<M::Phys>>(
        table_pa: Address<M::Phys, PgTableArray<T>>,
        va: Address<M, ()>,
        ctx: &mut WalkContext<PM>,
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
