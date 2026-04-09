use super::{
    alloc_order,
    allocator::{SlabAllocator, SlabManager},
};
use crate::{
    CpuOps,
    memory::{
        PAGE_SIZE,
        address::AddressTranslator,
        allocators::{phys::PageAllocGetter, slab::SLAB_MAX_OBJ_SHIFT},
        claimed_page::ClaimedPage,
    },
};
use core::mem::MaybeUninit;
use core::ptr;

const PTRS_PER_SZ_CLASS: usize = 32;
const NUM_PTR_CACHES: usize = SLAB_MAX_OBJ_SHIFT as usize + 1;

// Ensure that our cache fits in a single page.
const _: () = assert!(core::mem::size_of::<SlabCache>() <= PAGE_SIZE);

#[repr(C)]
pub struct PtrCache {
    next_free: usize,
    ptrs: [*mut u8; PTRS_PER_SZ_CLASS],
}

impl PtrCache {
    pub fn new() -> Self {
        Self {
            next_free: 0,
            ptrs: [ptr::null_mut(); PTRS_PER_SZ_CLASS],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.next_free == 0
    }

    pub fn is_full(&self) -> bool {
        self.next_free == PTRS_PER_SZ_CLASS
    }

    /// Caches as many allocations from the slab as possible.
    pub fn fill_from<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>>(
        &mut self,
        slab_alloc: &mut SlabManager<CPU, A, T>,
    ) {
        while !self.is_full()
            && let Some(ptr) = slab_alloc.try_alloc()
        {
            self.ptrs[self.next_free] = ptr;
            self.next_free += 1;
        }
    }

    /// Frees half of the cached allocations into `slab_alloc`.
    pub fn drain_into<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>>(
        &mut self,
        slab_alloc: &mut SlabManager<CPU, A, T>,
    ) {
        for _ in 0..(self.next_free >> 1) {
            self.next_free -= 1;
            slab_alloc.free(self.ptrs[self.next_free]);
        }
    }

    pub fn alloc(&mut self) -> Option<*mut u8> {
        if self.is_empty() {
            return None;
        }

        self.next_free -= 1;

        Some(self.ptrs[self.next_free])
    }

    /// Cache the allocation at `ptr`.
    ///
    /// # Returns
    /// - `Err(ptr)` if the cache is full.
    /// - `Ok(())` if `ptr` was cached
    pub fn free(&mut self, ptr: *mut u8) -> Result<(), *mut u8> {
        if self.is_full() {
            return Err(ptr);
        }

        self.ptrs[self.next_free] = ptr;
        self.next_free += 1;

        Ok(())
    }
}

impl Default for PtrCache {
    fn default() -> Self {
        Self::new()
    }
}

/// A slab cache.
///
/// Used for per-CPU (thereby lock-free) caching of allocations from slabs. The
/// size of the cache is guaranteed to be <= `PAGE_SIZE` thus can fit within a
/// [ClaimedPage].
#[repr(C)]
pub struct SlabCache {
    caches: [PtrCache; NUM_PTR_CACHES],
}

impl SlabCache {
    /// Initializes a SlabCache in-place on a claimed page.
    ///
    /// # Safety
    /// The caller must ensure the page is mapped and writable.
    /// The caller takes ownership of the returned pointer lifetime.
    pub unsafe fn from_page<CPU: CpuOps, G: PageAllocGetter<CPU>, T: AddressTranslator<()>>(
        page: ClaimedPage<CPU, G, T>,
    ) -> *mut Self {
        let ptr = page.va().cast::<SlabCache>().as_ptr_mut();

        let caches = unsafe {
            &mut *(&raw mut (*ptr).caches as *mut [MaybeUninit<PtrCache>; NUM_PTR_CACHES])
        };

        for elem in &mut caches[..] {
            elem.write(PtrCache::new());
        }

        // Leak the page so it isn't dropped (freed) at the end of this scope.
        page.leak();

        ptr
    }

    /// Helper to get the specific cache for a size index
    pub fn get_cache(&mut self, layout: core::alloc::Layout) -> Option<&mut PtrCache> {
        Some(&mut self.caches[alloc_order(layout)?])
    }

    /// Flush all cache lines back into the slab allocator.
    pub fn purge_into<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>>(
        &mut self,
        slab_alloc: &SlabAllocator<CPU, A, T>,
    ) {
        for (line, slab) in self.caches.iter_mut().zip(slab_alloc.managers.iter()) {
            let mut slab = slab.lock_save_irq();
            for i in 0..line.next_free {
                slab.free(line.ptrs[i]);
            }
            line.next_free = 0;
        }
    }
}
