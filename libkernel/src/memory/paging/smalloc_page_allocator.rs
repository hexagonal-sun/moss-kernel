//! A [`PageAllocator`] backed by a [`Smalloc`] allocator.

use crate::{
    error::Result,
    memory::{
        address::{AddressTranslator, TPA},
        allocators::smalloc::Smalloc,
        paging::{PageAllocator, PgTable, PgTableArray},
    },
};

/// A [`PageAllocator`] that satisfies page table allocations from a [`Smalloc`].
pub struct SmallocPageAlloc<'a, A: AddressTranslator<()>> {
    smalloc: &'a mut Smalloc<A>,
}

impl<'a, A: AddressTranslator<()>> SmallocPageAlloc<'a, A> {
    /// Creates a new `SmallocPageAlloc` wrapping the given [`Smalloc`].
    pub fn new(smalloc: &'a mut Smalloc<A>) -> Self {
        Self { smalloc }
    }
}

impl<A: AddressTranslator<()>> PageAllocator for SmallocPageAlloc<'_, A> {
    fn allocate_page_table<T: PgTable>(&mut self) -> Result<TPA<PgTableArray<T>>> {
        Ok(self.smalloc.alloc_page()?.pa().cast())
    }
}
