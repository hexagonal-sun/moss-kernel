//! Page-table tear-down and deallocation.

use crate::memory::{
    PAGE_SIZE,
    address::{PA, TPA, VA},
    paging::{
        PaMapper, PageTableEntry, PageTableMapper, PgTable, PgTableArray, TableMapper,
        TableMapperTable, walk::WalkContext,
    },
    region::{PhysMemoryRegion, VirtMemoryRegion},
};

/// Identifies the kind of physical frame a [`TeardownEntry`] describes.
pub enum EntryKind {
    /// A leaf page mapping.
    ///
    /// The [`VirtMemoryRegion`] covers the entire mapping virtual region.
    Mapping(VirtMemoryRegion),
    /// An intermediate page table frame (not the root).
    IntermediateTable,
    /// The root page table frame (PML4 / L0).
    RootTable,
}

/// Describes a single physical frame encountered during a page table tear-down.
pub struct TeardownEntry {
    /// What kind of frame this is.
    ///
    /// For [`EntryKind::Mapping`] entries the virtual region is embedded in the
    /// variant. [`EntryKind::IntermediateTable`] and [`EntryKind::RootTable`]
    /// entries describe infrastructure frames and carry no virtual-address
    /// information.
    pub kind: EntryKind,
    /// Depth in the page table tree. Root = 0, incrementing toward leaves.
    ///
    /// Intermediate table frames and the leaf mappings within the same table
    /// share the same depth value; [`EntryKind`] disambiguates.
    pub depth: u8,
    /// The physical memory region of this frame.
    pub region: PhysMemoryRegion,
}

/// The action the walker should take for a [`TeardownEntry`].
///
/// The `control` closure returns this to drive walker behaviour. The actual
/// freeing is always performed by the separate `deallocator` closure, called
/// by the walker in the correct order (after clearing if requested).
#[allow(missing_docs)]
pub enum TeardownAction {
    /// Recurse into this subtree (for [`EntryKind::IntermediateTable`] entries)
    /// and then call `deallocator` for its frame. For leaf entries, just call
    /// `deallocator`.
    Free,
    /// Like [`TeardownAction::Free`], but also zero the page table entry in the
    /// parent table *before* calling `deallocator`. The zeroing uses
    /// `write_volatile` and happens before the frame is released to the
    /// allocator, closing the stale-PTE window.
    FreeAndClear,
    /// Skip this entry entirely. For [`EntryKind::IntermediateTable`] entries,
    /// the subtree is not walked. `deallocator` is not called.
    Skip,
}

// Walker-internal discriminant used while scanning a table.
enum LocalEntryKind<NL: PgTable> {
    Table(TPA<PgTableArray<NL>>),
    Block(PA),
}

/// Trait implemented by each page table level that can be recursively walked
/// during a tear-down.
pub trait RecursiveTeardownWalker: PgTable + Sized {
    /// Walk all mappings in `table_pa` and invoke `control` / `deallocator`
    /// for each frame found.
    ///
    /// - `base_va`: the virtual address of the first byte covered by
    ///   `table_pa`'s first entry.
    /// - `depth`: the tree depth of `table_pa` itself (root's children = 1).
    /// - `control`: called for each entry *before* any recursion or
    ///   deallocation. Returns the action the walker should take. Must not
    ///   free physical memory — use `deallocator` for that.
    /// - `deallocator`: called by the walker (never inside a live
    ///   `with_page_table` window) to physically release a frame.
    fn tear_down<Control, Dealloc, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        base_va: VA,
        depth: u8,
        control: &mut Control,
        deallocator: &mut Dealloc,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        Control: FnMut(&TeardownEntry) -> TeardownAction,
        Dealloc: FnMut(PhysMemoryRegion);
}

/// Blanket impl for intermediate table levels whose descriptors support both
/// table and block mappings.
///
/// Root table levels (PML4, L0) are excluded because their descriptors cannot
/// encode block mappings — those are handled explicitly by
/// `tear_down_address_space`.
impl<T> RecursiveTeardownWalker for T
where
    T: TableMapperTable,
    T::Descriptor: PaMapper,
    <T::Descriptor as TableMapper>::NextLevel: RecursiveTeardownWalker,
{
    fn tear_down<Control, Dealloc, PM>(
        table_pa: TPA<PgTableArray<Self>>,
        ctx: &mut WalkContext<PM>,
        base_va: VA,
        depth: u8,
        control: &mut Control,
        deallocator: &mut Dealloc,
    ) -> crate::error::Result<()>
    where
        PM: PageTableMapper,
        Control: FnMut(&TeardownEntry) -> TeardownAction,
        Dealloc: FnMut(PhysMemoryRegion),
    {
        let mut cursor = 0;

        loop {
            let next_item: Option<(
                usize,
                LocalEntryKind<<T::Descriptor as TableMapper>::NextLevel>,
            )> = unsafe {
                ctx.mapper.with_page_table(table_pa, |pgtable| {
                    let table = Self::from_ptr(pgtable);
                    for i in cursor..<T as PgTable>::DESCRIPTORS_PER_PAGE {
                        let desc = table.get_idx(i);
                        if let Some(addr) = desc.next_table_address() {
                            return Some((i, LocalEntryKind::Table(addr)));
                        } else if let Some(pa) = desc.mapped_address() {
                            return Some((i, LocalEntryKind::Block(pa)));
                        }
                    }
                    None
                })?
            };

            match next_item {
                Some((found_idx, LocalEntryKind::Table(table_addr))) => {
                    let entry_va = base_va
                        .add_bytes(found_idx << <T::Descriptor as PageTableEntry>::MAP_SHIFT);
                    let child_depth = depth + 1;
                    let entry = TeardownEntry {
                        kind: EntryKind::IntermediateTable,
                        depth: child_depth,
                        region: PhysMemoryRegion::new(table_addr.to_untyped(), PAGE_SIZE),
                    };
                    let action = control(&entry);

                    if !matches!(action, TeardownAction::Skip) {
                        // Recurse into the child table first.
                        <T::Descriptor as TableMapper>::NextLevel::tear_down(
                            table_addr,
                            ctx,
                            entry_va,
                            child_depth,
                            control,
                            deallocator,
                        )?;

                        // Clear the PTE in this table before releasing the child
                        // frame, ensuring no stale translations outlive the frame.
                        if matches!(action, TeardownAction::FreeAndClear) {
                            unsafe {
                                ctx.mapper.with_page_table(table_pa, |pgtable| {
                                    Self::from_ptr(pgtable)
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

                Some((found_idx, LocalEntryKind::Block(pa))) => {
                    let entry_va = base_va
                        .add_bytes(found_idx << <T::Descriptor as PageTableEntry>::MAP_SHIFT);
                    let map_size = 1 << <T::Descriptor as PageTableEntry>::MAP_SHIFT;
                    let entry = TeardownEntry {
                        kind: EntryKind::Mapping(VirtMemoryRegion::new(entry_va, map_size)),
                        depth,
                        region: PhysMemoryRegion::new(pa, map_size),
                    };
                    let action = control(&entry);

                    if !matches!(action, TeardownAction::Skip) {
                        // Clear the block PTE before releasing the frame.
                        if matches!(action, TeardownAction::FreeAndClear) {
                            unsafe {
                                ctx.mapper.with_page_table(table_pa, |pgtable| {
                                    Self::from_ptr(pgtable)
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
