use crate::memory::{
    PAGE_SIZE,
    address::{PA, TPA},
    paging::{
        PaMapper, PageTableEntry, PageTableMapper, PgTable, PgTableArray, TableMapper,
        TableMapperTable, walk::WalkContext,
    },
    region::PhysMemoryRegion,
};

enum EntryKind<NL: PgTable> {
    Table(TPA<PgTableArray<NL>>),
    Block(PA),
}

pub trait RecursiveTeardownWalker: PgTable + Sized {
    fn tear_down<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        deallocator: &mut F,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(PhysMemoryRegion);
}

/// Blanket impl for intermediate table levels whose descriptors support both
/// table and block mappings (e.g. PDE/PDPE on x86_64, L1/L2 on arm64).
///
/// Root table levels (PML4, L0) are excluded because their descriptors cannot
/// encode block mappings — those are handled explicitly by `tear_down_address_space`.
impl<T> RecursiveTeardownWalker for T
where
    T: TableMapperTable,
    T::Descriptor: PaMapper,
    <T::Descriptor as TableMapper>::NextLevel: RecursiveTeardownWalker,
{
    fn tear_down<F, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        deallocator: &mut F,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        F: FnMut(PhysMemoryRegion),
    {
        let mut cursor = 0;

        loop {
            let next_item: Option<(usize, EntryKind<<T::Descriptor as TableMapper>::NextLevel>)> = unsafe {
                ctx.mapper.with_page_table(table_pa, |pgtable| {
                    let table = Self::from_ptr(pgtable);

                    for i in cursor..<T as PgTable>::DESCRIPTORS_PER_PAGE {
                        let desc = table.get_idx(i);

                        if let Some(addr) = desc.next_table_address() {
                            return Some((i, EntryKind::Table(addr)));
                        } else if let Some(pa) = desc.mapped_address() {
                            return Some((i, EntryKind::Block(pa)));
                        }
                    }
                    None
                })?
            };

            match next_item {
                Some((found_idx, EntryKind::Table(table_addr))) => {
                    // Recurse into the child table, then free its frame.
                    <T::Descriptor as TableMapper>::NextLevel::tear_down(
                        table_addr,
                        ctx,
                        deallocator,
                    )?;
                    deallocator(PhysMemoryRegion::new(table_addr.to_untyped(), PAGE_SIZE));
                    cursor = found_idx + 1;
                }
                Some((found_idx, EntryKind::Block(pa))) => {
                    // Block frame: size is the coverage of one entry at this level.
                    deallocator(PhysMemoryRegion::new(
                        pa,
                        1 << <T::Descriptor as PageTableEntry>::MAP_SHIFT,
                    ));
                    cursor = found_idx + 1;
                }
                None => break,
            }
        }

        Ok(())
    }
}
