use crate::memory::PAGE_SIZE;

// Allocations of order 2 (4 pages) from the FA for slabs.
pub(super) const SLAB_FRAME_ALLOC_ORDER: usize = 2;
pub(super) const SLAB_SIZE_BYTES: usize = PAGE_SIZE << SLAB_FRAME_ALLOC_ORDER;
const SLAB_MAX_OBJ_SHIFT: u32 = SLAB_SIZE_BYTES.ilog2() - 1;

#[allow(clippy::module_inception)]
pub(super) mod slab;
