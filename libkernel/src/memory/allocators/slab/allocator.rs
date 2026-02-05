/// A slab allocator for Moss.
use super::{
    SLAB_FRAME_ALLOC_ORDER, SLAB_MAX_OBJ_SHIFT, alloc_order,
    slab::{Slab, SlabState},
};
use crate::{
    CpuOps,
    memory::{
        address::{AddressTranslator, VA},
        allocators::{
            frame::{Frame, FrameAdapter, FrameList, FrameState},
            phys::PageAllocGetter,
        },
    },
    sync::spinlock::SpinLockIrq,
};
use core::marker::PhantomData;
use intrusive_collections::{LinkedList, UnsafeRef};

const MAX_FREE_SLABS: usize = 32;

/// Slab manager for a specific size class.
///
/// Manages a collection of slabs for a particular object size (size class). Two
/// main linked lists are managed: a 'free' list and a 'partial' list.
///
/// - The 'partial' list is a collection of slabs which are partially full. We
///   allocate from this list first for new allocations.
/// - The 'free' list is a collection of slabs which have no objects allocated
///   from them yet. We cache them in the hope that they will be used later on,
///   without the need to lock the FrameAllocator (FA) to get more physical
///   memory. When a particular size is reached (`MAX_FREE_SLABS`), we batch free
///   half of the slabs back to the FA.
///
/// # Full Slabs
///
/// There is no 'full' list. Full slabs are unlinked from both the 'partial' and
/// 'free' lists. They are allowed to float "in the ether" (referenced only by
/// the global [FrameList]). When freeing an object from a 'full' slab, the
/// allocator detects the state transition and re-links the frame into the
/// 'partial'/'free' list.
///
/// # Safety and Ownership
///
/// The FA and the `SlabManager` share a list of frame metadata via [FrameList]. To
/// share this list safely, we implement an implicit ownership model:
///
/// 1. When the FA allocates a frame, it initializes the metadata.
/// 2. We convert this frame into a slab allocation via
///    [PageAllocation::as_slab].
/// 3. Once that function returns, this `SlabManager` is considered the
///    exclusive owner of that frame's metadata.
///
/// It is therefore safe for the `SlabManager` to obtain a mutable reference to
/// the metadata of any frame in its possession, as the `SpinLock` protecting
/// this struct guarantees exclusive access to the specific size class that
/// "owns" the frame. Ownership is eventually returned to the FA via
/// [FrameAllocatorInner::free_slab].
pub struct SlabManager<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>> {
    free: LinkedList<FrameAdapter>,
    partial: LinkedList<FrameAdapter>,
    free_list_sz: usize,
    obj_shift: usize,
    frame_list: FrameList,
    phantom1: PhantomData<A>,
    phantom2: PhantomData<CPU>,
    phantom3: PhantomData<T>,
}

impl<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>> SlabManager<CPU, A, T> {
    fn new(obj_shift: usize, frame_list: FrameList) -> Self {
        Self {
            free: LinkedList::new(FrameAdapter::new()),
            partial: LinkedList::new(FrameAdapter::new()),
            free_list_sz: 0,
            obj_shift,
            frame_list,
            phantom1: PhantomData,
            phantom2: PhantomData,
            phantom3: PhantomData,
        }
    }

    /// Try to allocate a new object using free and partial slabs. Does *not*
    /// allocate any physical memory for the allocation.
    ///
    /// # Returns
    /// - `None` if there are no free or patial slabs available.
    /// - `Some(ptr)` if the allocation was successful.
    pub fn try_alloc(&mut self) -> Option<*mut u8> {
        // Lets start with partial list.
        if let Some(frame) = self.partial.pop_front().map(|x| {
            // SAFETY: We hold a mutable reference for self, therefore we can be
            // sure that we have exclusive access to all Frames owned by this
            // SlabAllocInner.
            unsafe { &mut *UnsafeRef::into_raw(x) }
        }) && let FrameState::Slab(ref mut slab) = frame.state
            && let Some(ptr) = slab.alloc_object()
        {
            if slab.state() == SlabState::Partial {
                // If the slab is still partial, re-insert back into the partial
                // list for further allocations.
                self.partial
                    .push_front(unsafe { UnsafeRef::from_raw(frame as *mut _) });
            }

            return Some(ptr);
        }

        if let Some(frame) = self.free.pop_front().map(|x| {
            // SAFETY: As above.
            unsafe { &mut *UnsafeRef::into_raw(x) }
        }) {
            let (ptr, state) = match frame.state {
                FrameState::Slab(ref mut slab) => {
                    let ptr = slab.alloc_object().unwrap();
                    let state = slab.state();

                    (ptr, state)
                }
                _ => unreachable!("Frame state should be slab"),
            };

            if state == SlabState::Partial {
                // SAFETY: The frame is now owned by the list and no other refs
                // to it will exist.
                self.partial
                    .push_front(unsafe { UnsafeRef::from_raw(frame as *const _) });
            }

            return Some(ptr);
        }

        None
    }

    /// Allocate an object for the given size class. Uses up partial and free
    /// slabs first; if none are avilable allocate a new slab from the frame
    /// allocator.
    pub fn alloc(&mut self) -> *mut u8 {
        // Fast path, first.
        if let Some(ptr) = self.try_alloc() {
            return ptr;
        }

        // Slow path, allocate a new frame.
        let new_alloc = A::global_page_alloc()
            .alloc_frames(SLAB_FRAME_ALLOC_ORDER as _)
            .expect("OOM - cannot allocate physical frame");

        let mut slab = Slab::new::<T, CPU>(&new_alloc, self.obj_shift);

        let obj = slab.alloc_object().expect("Slab should be empty");
        let state = slab.state();
        let frame = new_alloc.into_slab(slab);

        // We now have ownership of the frame.
        if state == SlabState::Partial {
            self.partial
                // SAFETY: Since we called `as_slab` above, we now have
                // exclusive ownership of the page frame.
                .push_front(unsafe { UnsafeRef::from_raw(frame) });
        }

        obj
    }

    /// Free the given allocation.
    pub fn free(&mut self, ptr: *mut u8) {
        // Find the frame.
        let va = VA::from_ptr_mut(ptr.cast());

        let frame = self.frame_list.get_frame(va.to_pa::<T>().to_pfn());

        let (frame, state) = {
            // Get the slab allocation data for this object.
            //
            // SAFETY: Since we hold the lock for slabs of this size class (&mut
            // self), we are guaranteed exclusive ownership of all slab frames
            // of this object size.
            fn do_free_obj(
                frame: *mut Frame,
                ptr: *mut u8,
                frame_list: &FrameList,
                obj_shift: usize,
            ) -> (*mut Frame, SlabState) {
                match (unsafe { &mut (*frame) }).state {
                    FrameState::AllocatedTail(ref tail_info) => {
                        let head_frame = frame_list.get_frame(tail_info.head);
                        do_free_obj(head_frame, ptr, frame_list, obj_shift)
                    }
                    FrameState::Slab(ref mut slab) => {
                        if slab.obj_shift() != obj_shift {
                            panic!("Slab allocator: Layout mismatch on free");
                        }
                        slab.put_object(ptr);
                        (frame, slab.state())
                    }
                    _ => unreachable!("Slab allocation"),
                }
            }

            do_free_obj(frame, ptr, &self.frame_list, self.obj_shift)
        };

        // SAFETY: As above
        let frame = unsafe { &mut *frame };

        match state {
            SlabState::Free => {
                if self.free_list_sz == MAX_FREE_SLABS {
                    let mut num_freed = 0;
                    let mut fa = A::global_page_alloc().inner.lock_save_irq();

                    // batch free some free slabs.
                    for _ in 0..(MAX_FREE_SLABS >> 1) {
                        let frame = self.free.pop_front().expect("Should have free slabs");

                        fa.free_slab(frame);

                        num_freed += 1;
                    }

                    self.free_list_sz -= num_freed;
                }

                if frame.link.is_linked() {
                    // The frame *must* be linked in the partial list if linked.
                    unsafe { self.partial.cursor_mut_from_ptr(frame as _) }.remove();
                }

                self.free
                    .push_front(unsafe { UnsafeRef::from_raw(frame as _) });

                self.free_list_sz += 1;
            }
            SlabState::Partial => {
                if !frame.link.is_linked() {
                    // We must have free'd an object on a previously full slab.
                    // Insert into the partial list.
                    self.partial
                        .push_front(unsafe { UnsafeRef::from_raw(frame as _) });
                }
            }
            SlabState::Full => unreachable!("we've just free'd an object"),
        }
    }
}

pub struct SlabAllocator<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>> {
    managers: [SpinLockIrq<SlabManager<CPU, A, T>, CPU>; SLAB_MAX_OBJ_SHIFT as usize + 1],
}

unsafe impl<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>> Send
    for SlabAllocator<CPU, A, T>
{
}
unsafe impl<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>> Sync
    for SlabAllocator<CPU, A, T>
{
}

impl<CPU: CpuOps, A: PageAllocGetter<CPU>, T: AddressTranslator<()>> SlabAllocator<CPU, A, T> {
    pub fn new(frame_list: FrameList) -> Self {
        Self {
            managers: core::array::from_fn(|n| {
                SpinLockIrq::new(SlabManager::new(n, frame_list.clone()))
            }),
        }
    }

    pub fn allocator_for_layout(
        &self,
        layout: core::alloc::Layout,
    ) -> Option<&SpinLockIrq<SlabManager<CPU, A, T>, CPU>> {
        Some(&self.managers[alloc_order(layout)?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{
            address::{IdentityTranslator, PA},
            allocators::{
                phys::{FrameAllocator, tests::TestFixture},
                slab::SLAB_SIZE_BYTES,
            },
        },
        sync::once_lock::OnceLock,
        test::MockCpuOps,
    };
    use core::alloc::Layout;

    type TstSlabAlloc = SlabAllocator<MockCpuOps, SlabTestAllocGetter, IdentityTranslator>;

    static FIXTURE: OnceLock<TestFixture, MockCpuOps> = OnceLock::new();

    struct SlabTestAllocGetter {}

    impl PageAllocGetter<MockCpuOps> for SlabTestAllocGetter {
        fn global_page_alloc() -> &'static FrameAllocator<MockCpuOps> {
            &FIXTURE.get().expect("Test not initalised").allocator
        }
    }

    /// Initializes the global allocator for the test suite.
    fn init_allocator() -> &'static TestFixture {
        FIXTURE.get_or_init(|| {
            // Allocate 32MB for the test heap to ensure we don't run out during large file tests
            TestFixture::new(&[(0, 32 * 1024 * 1024)], &[])
        })
    }

    fn create_allocator_fixture() -> TstSlabAlloc {
        let fixture = init_allocator();

        let frame_list = fixture.frame_list.clone();

        SlabAllocator::new(frame_list)
    }

    #[test]
    fn alloc_free_basic() {
        let allocator = create_allocator_fixture();

        // 64-byte allocation
        let layout = Layout::from_size_align(64, 64).unwrap();

        unsafe {
            let alloc = allocator.allocator_for_layout(layout).unwrap();
            let ptr = alloc.lock_save_irq().alloc();
            assert!(!ptr.is_null());
            assert_eq!(ptr as usize % 64, 0, "Alignment not respected");

            // Write to it to ensure valid pointer.
            *ptr = 0xAA;

            alloc.lock_save_irq().free(ptr);
        }
    }

    #[test]
    fn slab_creation_and_partial_list() {
        let allocator = create_allocator_fixture();

        // 1024 byte allocation.
        let layout = Layout::from_size_align(1024, 1024).unwrap();
        let alloc = allocator.allocator_for_layout(layout).unwrap();

        // Initial State: No slabs
        {
            let inner = alloc.lock_save_irq();
            assert!(inner.partial.is_empty());
            assert!(inner.free.is_empty());
        }

        // Alloc one object
        let ptr = alloc.lock_save_irq().alloc();

        {
            let inner = alloc.lock_save_irq();
            // Should now have 1 slab in partial
            assert_eq!(inner.partial.iter().count(), 1);
            assert!(inner.free.is_empty());

            assert!(matches!(
                unsafe {
                    &(*inner
                        .frame_list
                        .get_frame(PA::from_value(ptr as usize).to_pfn()))
                }
                .state,
                FrameState::Slab(_)
            ));
        }

        // Free the object
        alloc.lock_save_irq().free(ptr);

        {
            let inner = alloc.lock_save_irq();
            // Should move to Free list
            assert!(inner.partial.is_empty());
            assert_eq!(inner.free.iter().count(), 1);
        }
    }

    #[test]
    fn slab_exhaustion_and_floating_slabs() {
        let allocator = create_allocator_fixture();

        // Slab Capacity = 4 objects at 4k.
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        let alloc = allocator.allocator_for_layout(layout).unwrap();

        let mut ptrs = Vec::new();

        // Alloc 4 objects (Fill the first slab)
        {
            let mut alloc = alloc.lock_save_irq();
            for _ in 0..4 {
                ptrs.push(alloc.alloc());
            }
        }

        {
            let mut inner = alloc.lock_save_irq();
            // The slab is now full. It should have been removed from partial.
            assert!(inner.partial.is_empty());
            assert!(inner.free.is_empty());

            // try_alloc should return `None`.
            assert!(inner.try_alloc().is_none());
        }

        // Alloc 1 more object (Triggers new slab)
        let ptr_new = alloc.lock_save_irq().alloc();
        ptrs.push(ptr_new);

        {
            // Now we have a second slab, which is Partial (1/4 used)
            assert_eq!(alloc.lock_save_irq().partial.iter().count(), 1);
        }

        // Free an object from the first (Full/Floating) slab.
        let ptr_full_slab = ptrs[0];
        alloc.lock_save_irq().free(ptr_full_slab);

        {
            // Both slabs should now be in partial.
            assert_eq!(alloc.lock_save_irq().partial.iter().count(), 2);
        }
    }

    #[test]
    fn batch_freeing_threshold() {
        let allocator = create_allocator_fixture();

        let layout = Layout::from_size_align(64, 64).unwrap();
        let mut alloc = allocator
            .allocator_for_layout(layout)
            .unwrap()
            .lock_save_irq();

        let mut all_ptrs = Vec::new();

        // Create 33 separate slabs.
        let objs_per_slab = SLAB_SIZE_BYTES / 64;

        // Allocate 33 * 256 objects
        for _ in 0..(MAX_FREE_SLABS + 1) {
            for _ in 0..objs_per_slab {
                all_ptrs.push(alloc.alloc());
            }
        }

        // At this point, we have 33 full slabs. None are in partial/free lists.
        assert!(alloc.partial.is_empty());
        assert!(alloc.free.is_empty());

        // Free everything.
        // When we free the last object of a slab, it goes to Free list.
        // When Free list hits 32, the *next* one triggers a batch free (pops 16).
        for ptr in all_ptrs {
            alloc.free(ptr);
        }

        assert_eq!(alloc.free_list_sz, 17);
        assert_eq!(alloc.free.iter().count(), 17);
    }

    #[test]
    #[should_panic(expected = "Layout mismatch")]
    fn layout_mismatch_panic() {
        let allocator = create_allocator_fixture();

        // Alloc with size 64
        let layout_alloc = Layout::from_size_align(64, 64).unwrap();
        // Free with size 32 (Different lock, different inner allocator)
        let layout_free = Layout::from_size_align(32, 32).unwrap();

        let mut alloc_alloc = allocator
            .allocator_for_layout(layout_alloc)
            .unwrap()
            .lock_save_irq();
        let mut alloc_free = allocator
            .allocator_for_layout(layout_free)
            .unwrap()
            .lock_save_irq();

        let ptr = alloc_alloc.alloc();
        // This should panic because the slab metadata inside the page
        // says "Size 64", but we are calling free on the "Size 32" inner allocator.
        // The code has a check: `if slab.obj_shift() != obj_shift { panic! }`
        alloc_free.free(ptr);
    }
}
