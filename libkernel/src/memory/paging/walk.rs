//! Page-table walking functionality

use crate::{
    error::MapError,
    memory::{
        address::{TPA, VA},
        region::VirtMemoryRegion,
    },
};

use super::{
    PageTableEntry, PageTableMapper, PgTable, PgTableArray, TLBInvalidator, TableMapper,
    TableMapperTable,
};

/// A collection of context required to modify page tables.
pub struct WalkContext<'a, PM>
where
    PM: PageTableMapper + 'a,
{
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
                let sub_region = VirtMemoryRegion::new(entry_va, table_coverage)
                    .intersection(region)
                    .expect("Sub region should overlap with parent region");

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
