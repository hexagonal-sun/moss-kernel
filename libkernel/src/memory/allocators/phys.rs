//! Physical page-frame allocator (buddy allocator).

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

/// The maximum order for the buddy system. This corresponds to blocks of size
/// 2^MAX_ORDER pages.
pub const MAX_ORDER: usize = 10;

pub(super) struct FrameAllocatorInner {
    frame_list: FrameList,
    free_pages: usize,
    free_lists: [LinkedList<FrameAdapter>; MAX_ORDER + 1],
}

impl FrameAllocatorInner {
    pub(super) fn free_slab(&mut self, frame: UnsafeRef<Frame>) {
        assert!(matches!(frame.state, FrameState::Slab(_)));

        let pfn = frame.pfn;

        {
            // SAFETY: The caller should guarntee exclusive ownership of this frame
            // as it's being passed back to the FA.
            let frame = self.get_frame_mut(pfn);

            // Restore frame state data for slabs.
            frame.state = FrameState::AllocatedHead(AllocatedInfo {
                ref_count: 1,
                order: SLAB_FRAME_ALLOC_ORDER as _,
            });
        }

        self.free_frames(PhysMemoryRegion::new(pfn.pa(), SLAB_SIZE_BYTES));
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

    // Adds the MAX_ORDER-aligned blocks within `region` to the free lists.
    // The start address is aligned up to the next naturally aligned MAX_ORDER
    // boundary; any tail smaller than a single MAX_ORDER block is ignored.
    // Blocks whose head frame is already `Kernel` (e.g. due to a reservation
    // that overlaps the managed region) are left untouched and excluded from
    // the free lists.
    fn populate_free_region(&mut self, region: PhysMemoryRegion) {
        let aligned_start = region
            .start_address()
            .align_up(1 << (MAX_ORDER + PAGE_SHIFT));
        let end = region.end_address();

        if aligned_start >= end {
            return;
        }

        let mut current_pfn = aligned_start.to_pfn();
        let end_pfn = end.to_pfn();

        while current_pfn.value() + (1 << MAX_ORDER) <= end_pfn.value() {
            if !matches!(self.get_frame(current_pfn).state, FrameState::Kernel) {
                self.get_frame_mut(current_pfn).state = FrameState::Free {
                    order: MAX_ORDER as _,
                };
                self.add_to_free_list(current_pfn, MAX_ORDER);
                self.free_pages += 1 << MAX_ORDER;
            }
            current_pfn = PageFrame::from_pfn(current_pfn.value() + (1 << MAX_ORDER));
        }
    }
}

/// Thread-safe wrapper around the buddy frame allocator.
pub struct FrameAllocator<CPU: CpuOps> {
    pub(super) inner: SpinLockIrq<FrameAllocatorInner, CPU>,
}

/// An RAII guard for a contiguous allocation of physical page frames.
///
/// When dropped, the pages are automatically returned to the allocator.
pub struct PageAllocation<'a, CPU: CpuOps> {
    region: PhysMemoryRegion,
    inner: &'a SpinLockIrq<FrameAllocatorInner, CPU>,
}

impl<CPU: CpuOps> PageAllocation<'_, CPU> {
    /// Consumes the allocation without freeing it, returning the underlying region.
    pub fn leak(self) -> PhysMemoryRegion {
        let region = self.region;
        core::mem::forget(self);
        region
    }

    /// Returns a reference to the physical memory region backing this allocation.
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
    /// Use the entire span of all memory regions as the memory pool. This
    /// function takes ownership of `smalloc` since the buddy allocator will
    /// become the primary allocator for all memory.
    ///
    /// # Safety
    /// It's unsafe because it deals with raw pointers and takes ownership of
    /// the metadata memory. It should only be called once.
    pub unsafe fn init<T: AddressTranslator<()>>(mut smalloc: Smalloc<T>) -> (Self, FrameList) {
        // Find the entire memory span.
        let start = smalloc
            .base_ram_base_address()
            .expect("No memory regions in smalloc");

        let end = smalloc
            .iter_memory()
            .last()
            .expect("No memory regions in smalloc")
            .end_address();

        let (mut inner, frame_list) = Self::setup(
            &mut smalloc,
            PhysMemoryRegion::from_start_end_address(start, end),
        );

        for region in smalloc.iter_free() {
            inner.populate_free_region(region);
        }

        Self::finalize(inner, frame_list)
    }

    /// Initializes the frame allocator over a specific sub-region of physical
    /// memory. `region` must be a free, memory-backed region known to
    /// `smalloc`.
    ///
    /// Pre-existing reservations within `region` (e.g. the kernel image) are
    /// preserved and surface as `Kernel` frames in the resulting allocator.
    ///
    /// # Safety
    ///
    /// It's unsafe because it deals with raw pointers and takes ownership of
    /// the metadata memory. It should only be called once for the given region.
    pub unsafe fn init_from_region<T: AddressTranslator<()>>(
        smalloc: &mut Smalloc<T>,
        region: PhysMemoryRegion,
    ) -> (Self, FrameList) {
        smalloc
            .claim_region(region)
            .expect("init_from_region: region must be a free, memory-backed region");
        let (mut inner, frame_list) = Self::setup(smalloc, region);
        inner.populate_free_region(region);
        Self::finalize(inner, frame_list)
    }

    // Allocates the frame metadata out of `smalloc`, builds the inner allocator
    // state for `managed_region`, and marks any reserved regions overlapping it
    // as `Kernel` frames. The caller is responsible for populating the free
    // lists and finalizing.
    fn setup<T: AddressTranslator<()>>(
        smalloc: &mut Smalloc<T>,
        managed_region: PhysMemoryRegion,
    ) -> (FrameAllocatorInner, FrameList) {
        let lowest_addr = managed_region.start_address();
        let total_pages = managed_region.size() >> PAGE_SHIFT;
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

        for res_region in smalloc.res.iter() {
            if res_region.overlaps(managed_region) {
                for pfn in res_region.iter_pfns() {
                    if pfn >= base_page
                        && pfn.value() < base_page.value() + frame_list.total_pages()
                    {
                        allocator.get_frame_mut(pfn).state = FrameState::Kernel;
                    }
                }
            }
        }

        (allocator, frame_list)
    }

    // Wraps a fully-populated `FrameAllocatorInner` into a `FrameAllocator`,
    // emitting a log line summarising the resulting state.
    fn finalize(allocator: FrameAllocatorInner, frame_list: FrameList) -> (Self, FrameList) {
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

/// Provides access to the global page-frame allocator.
pub trait PageAllocGetter<C: CpuOps>: Send + Sync + 'static {
    /// Returns a reference to the global [`FrameAllocator`].
    fn global_page_alloc() -> &'static FrameAllocator<C>;
}

#[cfg(test)]
#[allow(missing_docs)]
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

        pub fn from_region(
            mem_regions: &[(usize, usize)],
            res_regions: &[(usize, usize)],
            managed: (usize, usize),
        ) -> Self {
            let total_size = mem_regions
                .iter()
                .map(|(start, size)| start + size)
                .max()
                .unwrap_or(16 * MIB);
            let layout =
                Layout::from_size_align(total_size, 1 << (MAX_ORDER + PAGE_SHIFT)).unwrap();
            let base_ptr = unsafe { std::alloc::alloc(layout) };
            assert!(!base_ptr.is_null(), "Test memory allocation failed");

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

            let managed_region =
                PhysMemoryRegion::new(PA::from_value(base_addr + managed.0), managed.1);
            // smalloc is dropped after this call; the frame metadata lives in the
            // backing allocation (base_ptr) which we retain until Drop.
            let (allocator, frame_list) =
                unsafe { FrameAllocator::init_from_region(&mut smalloc, managed_region) };

            Self {
                allocator,
                frame_list,
                base_ptr,
                layout,
            }
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

    #[test]
    fn init_from_region_basic() {
        let block_size = (1 << MAX_ORDER) * PAGE_SIZE;
        // Four blocks of memory; manage only the last two.
        let total_mem = 4 * block_size;
        let managed_offset = 2 * block_size;
        let managed_size = 2 * block_size;

        let fixture =
            TestFixture::from_region(&[(0, total_mem)], &[], (managed_offset, managed_size));

        let pages_in_two_blocks = 2 * (1 << MAX_ORDER);
        assert_eq!(fixture.frame_list.total_pages(), pages_in_two_blocks);
        assert_eq!(fixture.free_pages(), pages_in_two_blocks);

        // Only the MAX_ORDER free list should be populated, with two blocks.
        let mut expected_counts = [0usize; MAX_ORDER + 1];
        expected_counts[MAX_ORDER] = 2;
        fixture.assert_free_list_counts(&expected_counts);
    }

    #[test]
    fn init_from_region_base_page() {
        let block_size = (1 << MAX_ORDER) * PAGE_SIZE;
        let managed_offset = block_size;
        let managed_size = block_size;

        let fixture =
            TestFixture::from_region(&[(0, 2 * block_size)], &[], (managed_offset, managed_size));

        let backing_base = fixture.base_ptr as usize;
        let expected_base_pfn = (backing_base + managed_offset) / PAGE_SIZE;

        assert_eq!(
            fixture.frame_list.base_page().value(),
            expected_base_pfn,
            "base_page should be the start of the managed region"
        );
    }

    #[test]
    fn init_from_region_reservation_inside() {
        let block_size = (1 << MAX_ORDER) * PAGE_SIZE;
        // Manage the 2nd and 3rd blocks; reserve the head of the 3rd block.
        let managed_offset = block_size;
        let managed_size = 2 * block_size;
        let reserved_offset = managed_offset + block_size; // head of 2nd managed block

        let fixture = TestFixture::from_region(
            &[(0, 3 * block_size)],
            &[(reserved_offset, PAGE_SIZE)],
            (managed_offset, managed_size),
        );

        // The reserved frame at the block head should be Kernel.
        let backing_base = fixture.base_ptr as usize;
        let reserved_pfn = PageFrame::from_pfn((backing_base + reserved_offset) / PAGE_SIZE);
        assert!(
            matches!(fixture.frame_state(reserved_pfn), FrameState::Kernel),
            "reserved frame inside managed region should be Kernel"
        );

        // Only the non-reserved block should be free.
        let pages_per_block = 1 << MAX_ORDER;
        assert_eq!(
            fixture.free_pages(),
            pages_per_block,
            "block whose head is Kernel must be excluded from the free lists"
        );

        let mut expected_counts = [0usize; MAX_ORDER + 1];
        expected_counts[MAX_ORDER] = 1;
        fixture.assert_free_list_counts(&expected_counts);
    }

    #[test]
    fn init_from_region_reservation_outside() {
        let block_size = (1 << MAX_ORDER) * PAGE_SIZE;
        // Reserve something in the first block; manage only the second block.
        let reserved_offset = PAGE_SIZE; // in the first block
        let managed_offset = block_size;
        let managed_size = block_size;

        let fixture = TestFixture::from_region(
            &[(0, 2 * block_size)],
            &[(reserved_offset, PAGE_SIZE)],
            (managed_offset, managed_size),
        );

        // All managed pages should be free — the reservation is irrelevant.
        let pages_per_block = 1 << MAX_ORDER;
        assert_eq!(
            fixture.free_pages(),
            pages_per_block,
            "reservation outside the managed region must not reduce free pages"
        );

        let mut expected_counts = [0usize; MAX_ORDER + 1];
        expected_counts[MAX_ORDER] = 1;
        fixture.assert_free_list_counts(&expected_counts);
    }

    #[test]
    fn init_from_region_alloc_and_free() {
        let block_size = (1 << MAX_ORDER) * PAGE_SIZE;
        let fixture =
            TestFixture::from_region(&[(0, 2 * block_size)], &[], (block_size, block_size));

        let initial_free = fixture.free_pages();

        let alloc = fixture
            .allocator
            .alloc_frames(0)
            .expect("allocation within managed region should succeed");
        assert_eq!(fixture.free_pages(), initial_free - 1);

        drop(alloc);
        assert_eq!(
            fixture.free_pages(),
            initial_free,
            "memory should be fully recovered after free"
        );

        // And frames outside the managed region should be unreachable.
        let mut allocs = std::vec::Vec::new();
        while let Ok(a) = fixture.allocator.alloc_frames(0) {
            allocs.push(a);
        }
        assert_eq!(fixture.free_pages(), 0);
        assert_eq!(allocs.len(), initial_free);
    }

    #[test]
    fn init_from_region_unaligned_does_not_overshoot() {
        let block_size = (1 << MAX_ORDER) * PAGE_SIZE;
        // Backing memory: 4 MAX_ORDER blocks, all aligned.
        let total_mem = 4 * block_size;

        // Manage a region that starts one page into the 2nd block and extends
        // one page past the end of the 3rd block. After aligning the start
        // up to the 3rd block boundary, exactly one MAX_ORDER block fits.
        //
        // Without the fix, with_start_address would keep the original size
        // and extend end_address into block 4, erroneously populating a
        // second block that falls outside the managed region.
        let managed_offset = block_size + PAGE_SIZE;
        let managed_size = 2 * block_size;

        let fixture =
            TestFixture::from_region(&[(0, total_mem)], &[], (managed_offset, managed_size));

        // Only one MAX_ORDER block should be free (block 3). Block 2's head
        // is before managed_region.start, block 4 is past managed_region.end.
        let pages_per_block = 1 << MAX_ORDER;
        assert_eq!(
            fixture.free_pages(),
            pages_per_block,
            "only one full block fits after alignment; must not overshoot into block 4"
        );

        let mut expected_counts = [0usize; MAX_ORDER + 1];
        expected_counts[MAX_ORDER] = 1;
        fixture.assert_free_list_counts(&expected_counts);
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
