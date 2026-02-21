use super::{allocator::SlabAllocator, cache::SlabCache};
use crate::{
    CpuOps,
    memory::{
        PAGE_SIZE,
        address::{AddressTranslator, VA},
        allocators::phys::PageAllocGetter,
        page::ClaimedPage,
        region::PhysMemoryRegion,
    },
};
use core::{alloc::GlobalAlloc, marker::PhantomData, ops::DerefMut};

pub trait SlabGetter<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>> {
    fn global_slab_alloc() -> &'static SlabAllocator<CPU, A, T>;
}

pub trait SlabCacheStorage {
    fn store(ptr: *mut SlabCache);
    fn get() -> impl DerefMut<Target = SlabCache>;
}

pub struct KHeap<CPU, S, PG, T, SG>
where
    CPU: CpuOps,
    S: SlabCacheStorage,
    PG: PageAllocGetter<CPU>,
    T: AddressTranslator<()>,
    SG: SlabGetter<CPU, PG, T>,
{
    phantom1: PhantomData<S>,
    phantom2: PhantomData<PG>,
    phantom3: PhantomData<CPU>,
    phantom4: PhantomData<T>,
    phantom5: PhantomData<SG>,
}

impl<CPU, S, PG, T, SG> Default for KHeap<CPU, S, PG, T, SG>
where
    CPU: CpuOps,
    S: SlabCacheStorage,
    PG: PageAllocGetter<CPU>,
    T: AddressTranslator<()>,
    SG: SlabGetter<CPU, PG, T>,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<CPU, S, PG, T, SG> KHeap<CPU, S, PG, T, SG>
where
    CPU: CpuOps,
    S: SlabCacheStorage,
    PG: PageAllocGetter<CPU>,
    T: AddressTranslator<()>,
    SG: SlabGetter<CPU, PG, T>,
{
    pub const fn new() -> Self {
        Self {
            phantom1: PhantomData,
            phantom2: PhantomData,
            phantom3: PhantomData,
            phantom4: PhantomData,
            phantom5: PhantomData,
        }
    }

    /// Calculates the Frame Allocator order required for a large allocation.
    fn calculate_huge_order(layout: core::alloc::Layout) -> usize {
        // Ensure we cover the size, rounding UP to the nearest page.
        let size = core::cmp::max(layout.size(), layout.align());
        let pages_needed = size.div_ceil(PAGE_SIZE);
        pages_needed.next_power_of_two().ilog2() as usize
    }

    pub fn init_for_this_cpu() {
        let page: ClaimedPage<CPU, PG, T> =
            ClaimedPage::alloc_zeroed().expect("Cannot allocate heap page");

        // SAFETY: We just successfully allocated the above page and the
        // lifetime of the returned pointer will be for the entire lifetime of
        // the kernel ('sttaic).
        let slab_cache = unsafe { SlabCache::from_page(page) };

        // Store the slab_cache pointer in the storage.
        S::store(slab_cache);
    }
}

unsafe impl<CPU, S, PG, T, SG> GlobalAlloc for KHeap<CPU, S, PG, T, SG>
where
    CPU: CpuOps,
    S: SlabCacheStorage,
    PG: PageAllocGetter<CPU>,
    T: AddressTranslator<()>,
    SG: SlabGetter<CPU, PG, T>,
{
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let mut cache = S::get();

        let Some(cache_line) = cache.get_cache(layout) else {
            // Allocation is too big for SLAB. Defer to using the frame
            // allocator directly.
            return PG::global_page_alloc()
                .alloc_frames(Self::calculate_huge_order(layout) as _)
                .unwrap()
                .leak()
                .start_address()
                .to_va::<T>()
                .cast::<u8>()
                .as_ptr_mut();
        };

        if let Some(ptr) = cache_line.alloc() {
            // Fast path, cache-hit.
            return ptr;
        }

        // Fall back to the slab allocator.
        let mut slab = SG::global_slab_alloc()
            .allocator_for_layout(layout)
            .unwrap()
            .lock_save_irq();

        let ptr = slab.alloc();

        // Fill up our cache with objects from the (maybe freshly allocated)
        // slab.
        cache_line.fill_from(&mut slab);

        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        let mut cache = S::get();

        let Some(cache_line) = cache.get_cache(layout) else {
            // If the allocation didn't fit in the slab, we must have used the
            // FA directly.
            let allocated_region = PhysMemoryRegion::new(
                VA::from_ptr_mut(ptr as _).to_pa::<T>(),
                PAGE_SIZE << Self::calculate_huge_order(layout),
            );

            unsafe {
                PG::global_page_alloc().alloc_from_region(allocated_region);
            }

            return;
        };

        if cache_line.free(ptr).is_ok() {
            return;
        }

        // The cache is full. Return some memory back to the slab allocator.
        let mut slab = SG::global_slab_alloc()
            .allocator_for_layout(layout)
            .unwrap()
            .lock_save_irq();

        slab.free(ptr);

        cache_line.drain_into(&mut slab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{
            address::{IdentityTranslator, PA},
            allocators::{
                phys::{FrameAllocator, PageAllocGetter, tests::TestFixture},
                slab::{allocator::SlabAllocator, cache::SlabCache},
            },
        },
        test::MockCpuOps,
    };
    use rand::{Rng, rng};
    use std::{
        cell::RefCell,
        ops::{Deref, DerefMut},
        sync::{Arc, Barrier, OnceLock},
        thread,
    };

    static FIXTURE: OnceLock<TestFixture> = OnceLock::new();
    static SLAB_ALLOCATOR: OnceLock<
        SlabAllocator<MockCpuOps, TestAllocGetter, IdentityTranslator>,
    > = OnceLock::new();

    fn get_fixture() -> &'static TestFixture {
        FIXTURE.get_or_init(|| TestFixture::new(&[(0, 512 * 1024 * 1024)], &[]))
    }

    struct TestAllocGetter;
    impl PageAllocGetter<MockCpuOps> for TestAllocGetter {
        fn global_page_alloc() -> &'static FrameAllocator<MockCpuOps> {
            &get_fixture().allocator
        }
    }

    struct TestSlabGetter;
    impl SlabGetter<MockCpuOps, TestAllocGetter, IdentityTranslator> for TestSlabGetter {
        fn global_slab_alloc()
        -> &'static SlabAllocator<MockCpuOps, TestAllocGetter, IdentityTranslator> {
            SLAB_ALLOCATOR.get_or_init(|| {
                let fixture = get_fixture();
                SlabAllocator::new(fixture.frame_list.clone())
            })
        }
    }

    thread_local! {
        static TLS_CACHE: RefCell<Option<*mut SlabCache>> = RefCell::new(None);
    }

    struct ThreadLocalCacheStorage;

    struct ThreadCacheGuard {
        ptr: *mut SlabCache,
    }

    impl Deref for ThreadCacheGuard {
        type Target = SlabCache;
        fn deref(&self) -> &Self::Target {
            unsafe { &*self.ptr }
        }
    }

    impl DerefMut for ThreadCacheGuard {
        fn deref_mut(&mut self) -> &mut Self::Target {
            unsafe { &mut *self.ptr }
        }
    }

    impl SlabCacheStorage for ThreadLocalCacheStorage {
        fn store(ptr: *mut SlabCache) {
            TLS_CACHE.with(|c| {
                *c.borrow_mut() = Some(ptr);
            });
        }

        fn get() -> impl Deref<Target = SlabCache> + DerefMut {
            let ptr = TLS_CACHE.with(|c| {
                c.borrow()
                    .expect("Thread cache not initialized for this thread")
            });
            ThreadCacheGuard { ptr }
        }
    }

    type TestHeap = KHeap<
        MockCpuOps,
        ThreadLocalCacheStorage,
        TestAllocGetter,
        IdentityTranslator,
        TestSlabGetter,
    >;

    #[test]
    fn heap_stress_test() {
        let _ = get_fixture();
        let _ = TestSlabGetter::global_slab_alloc();

        for _ in 0..10 {
            let num_threads = 8;
            let ops_per_thread = 100_000;
            let barrier = Arc::new(Barrier::new(num_threads));

            // Track allocated memory usage to verify leak detection later
            let initial_free_pages = get_fixture().allocator.free_pages();
            println!("Initial Free Pages: {}", initial_free_pages);

            let mut handles = vec![];

            for t_idx in 0..num_threads {
                let barrier = barrier.clone();

                handles.push(thread::spawn(move || {
                    TestHeap::init_for_this_cpu();

                    barrier.wait();

                    let heap = TestHeap::new();
                    let mut rng = rng();

                    // Track allocations: (Ptr, Layout, PatternByte)
                    let mut allocations: Vec<(*mut u8, core::alloc::Layout, u8)> = Vec::new();

                    for _ in 0..ops_per_thread {
                        // Randomly decide to Alloc (70%) or Free (30%)
                        // Bias towards Alloc to build up memory pressure
                        if rng.random_bool(0.6) || allocations.is_empty() {
                            // Allocation path

                            // Random size: biased to small (slab), occasional huge
                            let size = 1024;

                            // Random alignment (power of 2)
                            let align = 1024;
                            let layout = core::alloc::Layout::from_size_align(size, align).unwrap();

                            unsafe {
                                let ptr = heap.alloc(layout);
                                assert!(!ptr.is_null(), "Allocation failed");
                                assert_eq!(ptr as usize % align, 0, "Alignment violation");

                                // Write Pattern
                                let pattern: u8 = rng.random();
                                std::ptr::write_bytes(ptr, pattern, size);

                                allocations.push((ptr, layout, pattern));
                            }
                        } else {
                            // Free Path.

                            // Remove a random allocation from our list
                            let idx = rng.random_range(0..allocations.len());
                            let (ptr, layout, pattern) = allocations.swap_remove(idx);

                            unsafe {
                                // Verify Pattern
                                let slice = std::slice::from_raw_parts(ptr, layout.size());
                                for (i, &byte) in slice.iter().enumerate() {
                                    assert_eq!(
                                        byte, pattern,
                                        "Memory Corruption detected in thread {} at byte {}",
                                        t_idx, i
                                    );
                                }

                                heap.dealloc(ptr, layout);
                            }
                        }
                    }

                    // Free everything.
                    for (ptr, layout, pattern) in allocations {
                        unsafe {
                            let slice = std::slice::from_raw_parts(ptr, layout.size());
                            for &byte in slice.iter() {
                                assert_eq!(byte, pattern, "Corruption detected during cleanup");
                            }
                            heap.dealloc(ptr, layout);
                        }
                    }

                    // Purge the per-cpu caches.
                    let slab = SLAB_ALLOCATOR.get().unwrap();
                    ThreadLocalCacheStorage::get().purge_into(&slab);

                    let addr = ThreadLocalCacheStorage::get().deref() as *const SlabCache;

                    // Return the slab cache page.
                    unsafe {
                        FIXTURE
                            .get()
                            .unwrap()
                            .allocator
                            .alloc_from_region(PhysMemoryRegion::new(
                                PA::from_value(addr as usize),
                                PAGE_SIZE,
                            ));
                    }
                }))
            }

            // Wait for all threads
            for h in handles {
                h.join().unwrap();
            }

            // Purge the all slab free lsts (the partial list should be empty).
            for slab_man in SLAB_ALLOCATOR.get().unwrap().managers.iter() {
                let mut frame_alloc = FIXTURE.get().unwrap().allocator.inner.lock_save_irq();

                let mut slab = slab_man.lock_save_irq();

                assert!(slab.partial.is_empty());

                while let Some(slab) = slab.free.pop_front() {
                    frame_alloc.free_slab(slab);
                }

                slab.free_list_sz = 0;
            }

            let final_free = get_fixture().allocator.free_pages();

            assert_eq!(initial_free_pages, final_free);
        }
    }
}
