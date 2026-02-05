use crate::{
    CpuOps,
    error::{KernelError, Result},
    memory::{
        PAGE_SHIFT,
        address::AddressTranslator,
        allocators::{
            frame::FrameState,
            slab::{SLAB_FRAME_ALLOC_ORDER, SLAB_SIZE_BYTES},
        },
        page::PageFrame,
        region::PhysMemoryRegion,
    },
    sync::spinlock::SpinLockIrq,
};
use core::{
    cmp::min,
    mem::{MaybeUninit, size_of, transmute},
};
use intrusive_collections::{LinkedList, UnsafeRef};
use log::info;

use super::{
    frame::{AllocatedInfo, Frame, FrameAdapter, FrameList, TailInfo},
    slab::slab::Slab,
    smalloc::Smalloc,
};

// The maximum order for the buddy system. This corresponds to blocks of size
// 2^MAX_ORDER pages.
const MAX_ORDER: usize = 10;

pub(super) struct FrameAllocatorInner {
    frame_list: FrameList,
    free_pages: usize,
    free_lists: [LinkedList<FrameAdapter>; MAX_ORDER + 1],
}

impl FrameAllocatorInner {
    pub(super) fn free_slab(&mut self, frame: UnsafeRef<Frame>) {
        assert!(matches!(frame.state, FrameState::Slab(_)));

        // SAFETY: The caller should guarntee exclusive ownership of this frame
        // as it's being passed back to the FA.
        let frame = unsafe { &mut *UnsafeRef::into_raw(frame) };

        // Restore frame state data for slabs.
        frame.state = FrameState::AllocatedHead(AllocatedInfo {
            ref_count: 1,
            order: SLAB_FRAME_ALLOC_ORDER as _,
        });

        self.free_frames(PhysMemoryRegion::new(frame.pfn.pa(), SLAB_SIZE_BYTES));
    }

    /// Frees a previously allocated block of frames.
    /// The PFN can point to any page within the allocated block.
    fn free_frames(&mut self, region: PhysMemoryRegion) {
        let head_pfn = region.start_address().to_pfn();

        debug_assert!(matches!(
            self.get_frame(head_pfn).state,
            FrameState::AllocatedHead(_)
        ));

        let initial_order =
            if let FrameState::AllocatedHead(ref mut info) = self.get_frame_mut(head_pfn).state {
                if info.ref_count > 1 {
                    info.ref_count -= 1;
                    return;
                }
                info.order as usize
            } else {
                unreachable!("Logic error: head PFN is not an AllocatedHead");
            };

        // Before merging, the block we're freeing is no longer allocated. Set
        // it to a temporary state. This prevents stale AllocatedHead states if
        // this block gets absorbed by its lower buddy.
        self.get_frame_mut(head_pfn).state = FrameState::Uninitialized;

        let mut merged_order = initial_order;
        let mut current_pfn = head_pfn;

        for order in initial_order..MAX_ORDER {
            let buddy_pfn = current_pfn.buddy(order);

            if buddy_pfn < self.frame_list.base_page()
                || buddy_pfn.value()
                    >= self.frame_list.base_page().value() + self.frame_list.total_pages()
            {
                break;
            }

            if let FrameState::Free { order: buddy_order } = self.get_frame(buddy_pfn).state
                && buddy_order as usize == order
            {
                // Buddy is free and of the same order. Merge them.

                // Remove the existing free buddy from its list. This function
                // already sets its state to Uninitialized.
                self.remove_from_free_list(buddy_pfn, order);

                // The new, larger block's PFN is the lower of the two.
                current_pfn = min(current_pfn, buddy_pfn);

                merged_order += 1;
            } else {
                break;
            }
        }

        // Update the state of the final merged block's head.
        self.get_frame_mut(current_pfn).state = FrameState::Free {
            order: merged_order as u8,
        }; // Add the correctly-stated block to the correct free list.
        self.add_to_free_list(current_pfn, merged_order);

        self.free_pages += 1 << initial_order;
    }

    #[inline]
    fn get_frame(&self, pfn: PageFrame) -> &Frame {
        unsafe { self.frame_list.get_frame(pfn).as_ref().unwrap() }
    }

    #[inline]
    fn get_frame_mut(&mut self, pfn: PageFrame) -> &mut Frame {
        unsafe { self.frame_list.get_frame(pfn).as_mut().unwrap() }
    }

    fn add_to_free_list(&mut self, pfn: PageFrame, order: usize) {
        #[cfg(test)]
        assert!(matches!(self.get_frame(pfn).state, FrameState::Free { .. }));

        self.free_lists[order]
            .push_front(unsafe { UnsafeRef::from_raw(self.get_frame(pfn) as *const _) });
    }

    fn remove_from_free_list(&mut self, pfn: PageFrame, order: usize) {
        let Some(_) = (unsafe {
            self.free_lists[order]
                .cursor_mut_from_ptr(self.get_frame(pfn) as *const _)
                .remove()
        }) else {
            panic!("Attempted to remove non-free block");
        };

        // Mark the removed frame as uninitialized to prevent dangling pointers.
        self.get_frame_mut(pfn).state = FrameState::Uninitialized;
    }
}

pub struct FrameAllocator<CPU: CpuOps> {
    pub(super) inner: SpinLockIrq<FrameAllocatorInner, CPU>,
}

pub struct PageAllocation<'a, CPU: CpuOps> {
    region: PhysMemoryRegion,
    inner: &'a SpinLockIrq<FrameAllocatorInner, CPU>,
}

impl<CPU: CpuOps> PageAllocation<'_, CPU> {
    pub fn leak(self) -> PhysMemoryRegion {
        let region = self.region;
        core::mem::forget(self);
        region
    }

    pub fn region(&self) -> &PhysMemoryRegion {
        &self.region
    }

    /// Leak the allocation as a slab allocation, for it to be picked back up
    /// again once the slab has been free'd.
    ///
    /// Returns an `UnsafeRef` to `Frame` that was converted to a slab for use
    /// in the slab lists.
    pub(super) fn into_slab(self, slab_info: Slab) -> *const Frame {
        let mut inner = self.inner.lock_save_irq();

        let frame = inner.get_frame_mut(self.region.start_address().to_pfn());

        debug_assert!(matches!(frame.state, FrameState::AllocatedHead(_)));

        frame.state = FrameState::Slab(slab_info);

        self.leak();

        frame as _
    }
}

impl<CPU: CpuOps> Clone for PageAllocation<'_, CPU> {
    fn clone(&self) -> Self {
        let mut inner = self.inner.lock_save_irq();

        match inner
            .get_frame_mut(self.region.start_address().to_pfn())
            .state
        {
            FrameState::AllocatedHead(ref mut alloc_info) => {
                alloc_info.ref_count += 1;
            }
            _ => panic!("Inconsistent memory metadata detected"),
        }

        Self {
            region: self.region,
            inner: self.inner,
        }
    }
}

impl<CPU: CpuOps> Drop for PageAllocation<'_, CPU> {
    fn drop(&mut self) {
        self.inner.lock_save_irq().free_frames(self.region);
    }
}
unsafe impl Send for FrameAllocatorInner {}

impl<CPU: CpuOps> FrameAllocator<CPU> {
    /// Allocates a physically contiguous block of frames.
    ///
    /// # Arguments
    /// * `order`: The order of the allocation, where the number of pages is `2^order`.
    ///   `order = 0` requests a single page.
    pub fn alloc_frames(&self, order: u8) -> Result<PageAllocation<'_, CPU>> {
        let mut inner = self.inner.lock_save_irq();
        let requested_order = order as usize;

        if requested_order > MAX_ORDER {
            return Err(KernelError::InvalidValue);
        }

        // Find the smallest order >= the requested order that has a free block.
        let Some((free_block, mut current_order)) =
            (requested_order..=MAX_ORDER).find_map(|order| {
                let pg_block = inner.free_lists[order].pop_front()?;
                Some((pg_block, order))
            })
        else {
            return Err(KernelError::NoMemory);
        };

        let free_block = inner.get_frame_mut(free_block.pfn);

        free_block.state = FrameState::Uninitialized;
        let block_pfn = free_block.pfn;

        // Split the block down until it's the correct size.
        while current_order > requested_order {
            current_order -= 1;
            let buddy = block_pfn.buddy(current_order);
            inner.get_frame_mut(buddy).state = FrameState::Free {
                order: current_order as _,
            };
            inner.add_to_free_list(buddy, current_order);
        }

        // Mark the final block metadata.
        inner.get_frame_mut(block_pfn).state = FrameState::AllocatedHead(AllocatedInfo {
            ref_count: 1,
            order: requested_order as u8,
        });

        let num_pages_in_block = 1 << requested_order;

        for i in 1..num_pages_in_block {
            inner.get_frame_mut(block_pfn.add_pages(i)).state =
                FrameState::AllocatedTail(TailInfo { head: block_pfn });
        }

        inner.free_pages -= num_pages_in_block;

        Ok(PageAllocation {
            region: PhysMemoryRegion::new(block_pfn.pa(), num_pages_in_block << PAGE_SHIFT),
            inner: &self.inner,
        })
    }

    /// Constructs an allocation from a phys mem region.
    ///
    /// # Safety
    ///
    /// This function does no checks to ensure that the region passed is
    /// actually allocated and the region is of the correct size. The *only* way
    /// to ensure safety is to use a region that was previously leaked with
    /// [PageAllocation::leak].
    pub unsafe fn alloc_from_region(&self, region: PhysMemoryRegion) -> PageAllocation<'_, CPU> {
        PageAllocation {
            region,
            inner: &self.inner,
        }
    }

    /// Returns `true` if the page is part of an allocated block, `false`
    /// otherwise.
    pub fn is_allocated(&self, pfn: PageFrame) -> bool {
        matches!(
            self.inner.lock_save_irq().get_frame(pfn).state,
            FrameState::AllocatedHead(_) | FrameState::AllocatedTail(_)
        )
    }

    /// Returns `true` if the page is part of an allocated block and has a ref
    /// count of 1, `false` otherwise.
    pub fn is_allocated_exclusive(&self, mut pfn: PageFrame) -> bool {
        let inner = self.inner.lock_save_irq();

        loop {
            match inner.get_frame(pfn).state {
                FrameState::AllocatedTail(TailInfo { head }) => pfn = head,
                FrameState::AllocatedHead(AllocatedInfo { ref_count: 1, .. }) => {
                    return true;
                }
                _ => return false,
            }
        }
    }

    /// Returns the total number of pages managed by this allocator.
    #[inline]
    pub fn total_pages(&self) -> usize {
        self.inner.lock_save_irq().frame_list.total_pages()
    }

    /// Returns the current number of free pages available for allocation.
    #[inline]
    pub fn free_pages(&self) -> usize {
        self.inner.lock_save_irq().free_pages
    }

    /// Initializes the frame allocator. This is the main bootstrap function.
    ///
    /// # Safety
    /// It's unsafe because it deals with raw pointers and takes ownership of
    /// the metadata memory. It should only be called once.
    pub unsafe fn init<T: AddressTranslator<()>>(mut smalloc: Smalloc<T>) -> (Self, FrameList) {
        let highest_addr = smalloc
            .iter_memory()
            .map(|r| r.end_address())
            .max()
            .unwrap();

        let lowest_addr = smalloc
            .iter_memory()
            .map(|r| r.start_address())
            .min()
            .unwrap();

        let total_pages = (highest_addr.value() - lowest_addr.value()) >> PAGE_SHIFT;
        let metadata_size = total_pages * size_of::<Frame>();

        let metadata_addr = smalloc
            .alloc(metadata_size, align_of::<Frame>())
            .expect("Failed to allocate memory for page metadata")
            .cast::<MaybeUninit<Frame>>();

        let pages_list_uninit: &mut [MaybeUninit<Frame>] = unsafe {
            core::slice::from_raw_parts_mut(
                metadata_addr.to_untyped().to_va::<T>().cast().as_ptr_mut(),
                total_pages,
            )
        };

        // Initialize all frames to a known state.
        for (i, p) in pages_list_uninit.iter_mut().enumerate() {
            p.write(Frame::new(PageFrame::from_pfn(
                lowest_addr.to_pfn().value() + i,
            )));
        }

        // The transmute is safe because we just initialized all elements.
        let pages: &mut [Frame] = unsafe { transmute(pages_list_uninit) };

        let base_page = lowest_addr.to_pfn();

        // SAFETY: We can only call this funcion once since it consumes smalloc
        // on kernel boot. Therfore, we can only initialize the frmae list only
        // once. Furthermore, we've just reseved the memory needed for the
        // framelst from smalloc and initalised it with valid values.
        let frame_list = unsafe { FrameList::new(pages, base_page) };

        let mut allocator = FrameAllocatorInner {
            frame_list: frame_list.clone(),
            free_pages: 0,
            free_lists: core::array::from_fn(|_| LinkedList::new(FrameAdapter::new())),
        };

        for region in smalloc.res.iter() {
            for pfn in region.iter_pfns() {
                if pfn >= base_page && pfn.value() < base_page.value() + frame_list.total_pages() {
                    allocator.get_frame_mut(pfn).state = FrameState::Kernel;
                }
            }
        }

        for region in smalloc.iter_free() {
            // Align the start address to the first naturally aligned MAX_ORDER
            // block.
            let region =
                region.with_start_address(region.start_address().align_up(1 << (MAX_ORDER + 12)));

            let mut current_pfn = region.start_address().to_pfn();
            let end_pfn = region.end_address().to_pfn();

            while current_pfn.value() + (1 << MAX_ORDER) <= end_pfn.value() {
                allocator.get_frame_mut(current_pfn).state = FrameState::Free {
                    order: MAX_ORDER as _,
                };
                allocator.add_to_free_list(current_pfn, MAX_ORDER);
                allocator.free_pages += 1 << MAX_ORDER;
                current_pfn = PageFrame::from_pfn(current_pfn.value() + (1 << MAX_ORDER));
            }
        }

        info!(
            "Buddy allocator initialized. Managing {} pages, {} free.",
            frame_list.total_pages(),
            allocator.free_pages
        );

        (
            FrameAllocator {
                inner: SpinLockIrq::new(allocator),
            },
            frame_list,
        )
    }
}

pub trait PageAllocGetter<C: CpuOps>: Send + Sync + 'static {
    fn global_page_alloc() -> &'static FrameAllocator<C>;
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::{
        memory::{
            address::{IdentityTranslator, PA},
            allocators::smalloc::RegionList,
            region::PhysMemoryRegion,
        },
        test::MockCpuOps,
    };
    use core::{alloc::Layout, mem::MaybeUninit};
    use std::{mem::ManuallyDrop, ptr, vec::Vec}; // For collecting results in tests

    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    const PAGE_SIZE: usize = 4096;

    pub struct TestFixture {
        pub allocator: FrameAllocator<MockCpuOps>,
        pub frame_list: FrameList,
        base_ptr: *mut u8,
        layout: Layout,
    }

    unsafe impl Send for TestFixture {}
    unsafe impl Sync for TestFixture {}

    impl TestFixture {
        /// Creates a new test fixture.
        ///
        /// - `mem_regions`: A slice of `(start, size)` tuples defining available memory regions.
        ///   The `start` is relative to the beginning of the allocated memory block.
        /// - `res_regions`: A slice of `(start, size)` tuples for reserved regions (e.g., kernel).
        pub fn new(mem_regions: &[(usize, usize)], res_regions: &[(usize, usize)]) -> Self {
            // Determine the total memory size required for the test environment.
            let total_size = mem_regions
                .iter()
                .map(|(start, size)| start + size)
                .max()
                .unwrap_or(16 * MIB);
            let layout =
                Layout::from_size_align(total_size, 1 << (MAX_ORDER + PAGE_SHIFT)).unwrap();
            let base_ptr = unsafe { std::alloc::alloc(layout) };
            assert!(!base_ptr.is_null(), "Test memory allocation failed");

            // Leaking is a common pattern in kernel test code to get static slices.
            let mem_region_list: &mut [MaybeUninit<PhysMemoryRegion>] =
                Vec::from([MaybeUninit::uninit(); 16]).leak();
            let res_region_list: &mut [MaybeUninit<PhysMemoryRegion>] =
                Vec::from([MaybeUninit::uninit(); 16]).leak();

            let mut smalloc: Smalloc<IdentityTranslator> = Smalloc::new(
                RegionList::new(16, mem_region_list.as_mut_ptr().cast()),
                RegionList::new(16, res_region_list.as_mut_ptr().cast()),
            );

            let base_addr = base_ptr as usize;

            for &(start, size) in mem_regions {
                smalloc
                    .add_memory(PhysMemoryRegion::new(
                        PA::from_value(base_addr + start),
                        size,
                    ))
                    .unwrap();
            }
            for &(start, size) in res_regions {
                smalloc
                    .add_reservation(PhysMemoryRegion::new(
                        PA::from_value(base_addr + start),
                        size,
                    ))
                    .unwrap();
            }

            let (allocator, frame_list) = unsafe { FrameAllocator::init(smalloc) };

            Self {
                allocator,
                frame_list,
                base_ptr,
                layout,
            }
        }

        /// Get the state of a specific frame.
        fn frame_state(&self, pfn: PageFrame) -> FrameState {
            self.allocator
                .inner
                .lock_save_irq()
                .get_frame(pfn)
                .state
                .clone()
        }

        /// Checks that the number of blocks in each free list matches the expected counts.
        fn assert_free_list_counts(&self, expected_counts: &[usize; MAX_ORDER + 1]) {
            for order in 0..=MAX_ORDER {
                let count = self.allocator.inner.lock_save_irq().free_lists[order]
                    .iter()
                    .count();
                assert_eq!(
                    count, expected_counts[order],
                    "Mismatch in free list count for order {}",
                    order
                );
            }
        }

        fn free_pages(&self) -> usize {
            self.allocator.inner.lock_save_irq().free_pages
        }

        pub fn leak_allocator(self) -> FrameAllocator<MockCpuOps> {
            let this = ManuallyDrop::new(self);

            unsafe { ptr::read(&this.allocator) }
        }
    }

    impl Drop for TestFixture {
        fn drop(&mut self) {
            unsafe {
                self.allocator
                    .inner
                    .lock_save_irq()
                    .free_lists
                    .iter_mut()
                    .for_each(|x| x.clear());

                std::alloc::dealloc(self.base_ptr, self.layout);
            }
        }
    }

    /// Tests basic allocator initialization with a single large, contiguous memory region.
    #[test]
    fn init_simple() {
        let fixture = TestFixture::new(&[(0, (1 << (MAX_ORDER + PAGE_SHIFT)) * 2)], &[]);
        let pages_in_max_block = 1 << MAX_ORDER;

        assert_eq!(fixture.free_pages(), pages_in_max_block);
        assert!(!fixture.allocator.inner.lock_save_irq().free_lists[MAX_ORDER].is_empty());

        // Check that all other lists are empty
        for i in 0..MAX_ORDER {
            assert!(fixture.allocator.inner.lock_save_irq().free_lists[i].is_empty());
        }
    }

    #[test]
    fn init_with_kernel_reserved() {
        let block_size = (1 << MAX_ORDER) * PAGE_SIZE;
        // A region large enough for 3 max-order blocks
        let total_size = 4 * block_size;

        // Reserve the middle block. Even a single page anywhere in that block
        // should wipe out the whole block.
        let res_regions = &[(block_size * 2 + 4 * PAGE_SIZE, PAGE_SIZE)];
        let fixture = TestFixture::new(&[(0, total_size)], res_regions);

        let pages_in_max_block = 1 << MAX_ORDER;
        // We should have 2 max-order blocks, not 3.
        assert_eq!(fixture.free_pages(), 2 * pages_in_max_block);

        // The middle pages should be marked as Kernel
        let reserved_pfn = PageFrame::from_pfn(
            fixture
                .allocator
                .inner
                .lock_save_irq()
                .frame_list
                .base_page()
                .value()
                + (pages_in_max_block * 2 + 4),
        );

        assert!(matches!(
            fixture.frame_state(reserved_pfn),
            FrameState::Kernel
        ));

        // Allocation of a MAX_ORDER block should succeed twice.
        fixture
            .allocator
            .alloc_frames(MAX_ORDER as u8)
            .unwrap()
            .leak();
        fixture
            .allocator
            .alloc_frames(MAX_ORDER as u8)
            .unwrap()
            .leak();

        // A third should fail.
        assert!(matches!(
            fixture.allocator.alloc_frames(MAX_ORDER as u8),
            Err(KernelError::NoMemory)
        ));
    }

    /// Tests a simple allocation and deallocation cycle.
    #[test]
    fn simple_alloc_and_free() {
        let fixture = TestFixture::new(&[(0, (1 << (MAX_ORDER + PAGE_SHIFT)) * 2)], &[]);
        let initial_free_pages = fixture.free_pages();

        // Ensure we start with a single MAX_ORDER block.
        let mut expected_counts = [0; MAX_ORDER + 1];
        expected_counts[MAX_ORDER] = 1;
        fixture.assert_free_list_counts(&expected_counts);

        // Allocate a single page
        let alloc = fixture
            .allocator
            .alloc_frames(0)
            .expect("Allocation failed");
        assert_eq!(fixture.free_pages(), initial_free_pages - 1);

        // Check its state
        match fixture.frame_state(alloc.region.start_address().to_pfn()) {
            FrameState::AllocatedHead(info) => {
                assert_eq!(info.order, 0);
                assert_eq!(info.ref_count, 1);
            }
            _ => panic!("Incorrect frame state after allocation"),
        }

        // Free the page
        drop(alloc);
        assert_eq!(fixture.free_pages(), initial_free_pages);

        // Ensure we merged back to a single MAX_ORDER block.
        fixture.assert_free_list_counts(&expected_counts);
    }

    /// Tests allocation that requires splitting a large block.
    #[test]
    fn alloc_requires_split() {
        let fixture = TestFixture::new(&[(0, (1 << (MAX_ORDER + PAGE_SHIFT)) * 2)], &[]);

        // Allocate a single page (order 0)
        let _pfn = fixture.allocator.alloc_frames(0).unwrap();

        // Check free pages
        let pages_in_block = 1 << MAX_ORDER;
        assert_eq!(fixture.free_pages(), pages_in_block - 1);

        // Splitting a MAX_ORDER block to get an order 0 page should leave
        // one free block at each intermediate order.
        let mut expected_counts = [0; MAX_ORDER + 1];
        for i in 0..MAX_ORDER {
            expected_counts[i] = 1;
        }
        fixture.assert_free_list_counts(&expected_counts);
    }

    /// Tests the allocation of a multipage block and verifies head/tail metadata.
    #[test]
    fn alloc_multi_page_block() {
        let fixture = TestFixture::new(&[(0, (1 << (MAX_ORDER + PAGE_SHIFT)) * 2)], &[]);
        let order = 3; // 8 pages

        let head_region = fixture.allocator.alloc_frames(order).unwrap();
        assert_eq!(head_region.region.iter_pfns().count(), 8);

        // Check head page
        match fixture.frame_state(head_region.region.iter_pfns().next().unwrap()) {
            FrameState::AllocatedHead(info) => assert_eq!(info.order, order as u8),
            _ => panic!("Head page has incorrect state"),
        }

        // Check tail pages
        for (i, pfn) in head_region.region.iter_pfns().skip(1).enumerate() {
            match fixture.frame_state(pfn) {
                FrameState::AllocatedTail(info) => {
                    assert_eq!(info.head, head_region.region.start_address().to_pfn())
                }
                _ => panic!("Tail page {} has incorrect state", i),
            }
        }
    }

    /// Tests that freeing a tail page correctly frees the entire block.
    #[test]
    fn free_tail_page() {
        let fixture = TestFixture::new(&[(0, (1 << (MAX_ORDER + PAGE_SHIFT)) * 2)], &[]);
        let initial_free = fixture.free_pages();
        let order = 4; // 16 pages
        let num_pages = 1 << order;

        let head_alloc = fixture.allocator.alloc_frames(order as u8).unwrap();
        assert_eq!(fixture.free_pages(), initial_free - num_pages);

        drop(head_alloc);

        // All pages should be free again
        assert_eq!(fixture.free_pages(), initial_free);
    }

    /// Tests exhausting memory and handling the out-of-memory condition.
    #[test]
    fn alloc_out_of_memory() {
        let fixture = TestFixture::new(&[(0, (1 << (MAX_ORDER + PAGE_SHIFT)) * 2)], &[]);
        let total_pages = fixture.free_pages();
        assert!(total_pages > 0);

        let mut allocs = Vec::new();
        for _ in 0..total_pages {
            match fixture.allocator.alloc_frames(0) {
                Ok(pfn) => allocs.push(pfn),
                Err(e) => panic!("Allocation failed prematurely: {:?}", e),
            }
        }

        assert_eq!(fixture.free_pages(), 0);

        // Next allocation should fail
        let result = fixture.allocator.alloc_frames(0);
        assert!(matches!(result, Err(KernelError::NoMemory)));

        // Free everything and check if memory is recovered
        drop(allocs);

        assert_eq!(fixture.free_pages(), total_pages);
    }

    /// Tests that requesting an invalid order fails gracefully.
    #[test]
    fn alloc_invalid_order() {
        let fixture = TestFixture::new(&[(0, (1 << (MAX_ORDER + PAGE_SHIFT)) * 2)], &[]);
        let result = fixture.allocator.alloc_frames((MAX_ORDER + 1) as u8);
        assert!(matches!(result, Err(KernelError::InvalidValue)));
    }

    /// Tests the reference counting mechanism in `free_frames`.
    #[test]
    fn ref_count_free() {
        let fixture = TestFixture::new(&[(0, (1 << (MAX_ORDER + PAGE_SHIFT)) * 2)], &[]);
        let initial_free = fixture.free_pages();

        let alloc1 = fixture.allocator.alloc_frames(2).unwrap();
        let alloc2 = alloc1.clone();
        let alloc3 = alloc2.clone();

        let pages_in_block = 1 << 2;
        assert_eq!(fixture.free_pages(), initial_free - pages_in_block);

        let pfn = alloc1.region().start_address().to_pfn();

        // First free should just decrement the count
        drop(alloc1);

        assert_eq!(fixture.free_pages(), initial_free - pages_in_block);
        if let FrameState::AllocatedHead(info) = fixture.frame_state(pfn) {
            assert_eq!(info.ref_count, 2);
        } else {
            panic!("Page state changed unexpectedly");
        }

        // Second free, same thing
        drop(alloc2);

        assert_eq!(fixture.free_pages(), initial_free - pages_in_block);
        if let FrameState::AllocatedHead(info) = fixture.frame_state(pfn) {
            assert_eq!(info.ref_count, 1);
        } else {
            panic!("Page state changed unexpectedly");
        }

        // Third free should actually release the memory
        drop(alloc3);
        assert_eq!(fixture.free_pages(), initial_free);
        assert!(matches!(fixture.frame_state(pfn), FrameState::Free { .. }));
    }
}
