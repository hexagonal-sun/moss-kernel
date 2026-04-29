use crate::{
    arch::ArchImpl,
    memory::{PageOffsetTranslator, page::PgAllocGetter},
    sync::OnceLock,
};
use core::{
    arch::asm,
    ops::{Deref, DerefMut},
    ptr,
};
use libkernel::{
    CpuOps,
    memory::allocators::slab::{
        allocator::SlabAllocator,
        cache::SlabCache,
        heap::{KHeap, SlabCacheStorage, SlabGetter},
    },
};

type SlabAlloc = SlabAllocator<ArchImpl, PgAllocGetter, PageOffsetTranslator>;

pub static SLAB_ALLOC: OnceLock<SlabAlloc> = OnceLock::new();

pub struct StaticSlabGetter {}

impl SlabGetter<ArchImpl, PgAllocGetter, PageOffsetTranslator> for StaticSlabGetter {
    fn global_slab_alloc() -> &'static SlabAlloc {
        SLAB_ALLOC.get().unwrap()
    }
}

pub struct PerCpuCache {
    flags: u64,
}

impl PerCpuCache {
    fn get_ptr() -> *mut SlabCache {
        let mut cache: *mut SlabCache = ptr::null_mut();

        unsafe { asm!("mrs {}, TPIDR_EL1", out(reg) cache, options(nostack, nomem)) };

        if cache.is_null() {
            panic!("Attempted to use alloc/free before CPU initalisation!");
        }

        cache
    }
}

impl SlabCacheStorage for PerCpuCache {
    fn store(ptr: *mut SlabCache) {
        #[allow(clippy::pointers_in_nomem_asm_block)]
        unsafe {
            asm!("msr TPIDR_EL1, {}", in(reg) ptr, options(nostack, nomem));
        };
    }

    fn get() -> impl DerefMut<Target = SlabCache> {
        let flags = ArchImpl::disable_interrupts();

        Self { flags }
    }
}

impl Deref for PerCpuCache {
    type Target = SlabCache;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The pointer uses a CPU-banked register for access. We've
        // disabled interrupts so we know we cannot be preempted, therefore
        // mutable access to the cache is safe.
        unsafe { &(*Self::get_ptr()) }
    }
}

impl DerefMut for PerCpuCache {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: The pointer uses a CPU-banked register for access. We've
        // disabled interrupts so we know we cannot be preempted, therefore
        // mutable access to the cache is safe.
        unsafe { &mut (*Self::get_ptr()) }
    }
}

impl Drop for PerCpuCache {
    fn drop(&mut self) {
        ArchImpl::restore_interrupt_state(self.flags);
    }
}

pub type KernelHeap =
    KHeap<ArchImpl, PerCpuCache, PgAllocGetter, PageOffsetTranslator, StaticSlabGetter>;

#[global_allocator]
static K_HEAP: KernelHeap = KernelHeap::new();
