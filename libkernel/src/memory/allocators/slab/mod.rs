use crate::memory::PAGE_SIZE;

// Allocations of order 2 (4 pages) from the FA for slabs.
pub(super) const SLAB_FRAME_ALLOC_ORDER: usize = 2;
pub(super) const SLAB_SIZE_BYTES: usize = PAGE_SIZE << SLAB_FRAME_ALLOC_ORDER;
const SLAB_MAX_OBJ_SHIFT: u32 = SLAB_SIZE_BYTES.ilog2() - 1;

pub mod allocator;
#[allow(clippy::module_inception)]
pub(super) mod slab;

/// Returns the index into the slab/cache list for a given layout.
fn alloc_order(layout: core::alloc::Layout) -> Option<usize> {
    // We must take alignemnt into account too.
    let size = core::cmp::max(layout.size(), layout.align());

    let alloc_order = size.next_power_of_two().ilog2() as usize;

    if alloc_order > SLAB_MAX_OBJ_SHIFT as usize {
        return None;
    }

    // Since slabs use a `u16` as the 'next_free' pointer, our minimum order
    // must be 1.
    Some(if alloc_order == 0 { 1 } else { alloc_order })
}
