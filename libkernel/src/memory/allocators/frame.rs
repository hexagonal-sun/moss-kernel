use crate::memory::page::PageFrame;
use intrusive_collections::{LinkedListLink, UnsafeRef, intrusive_adapter};

use super::slab::slab::Slab;

#[derive(Clone, Copy, Debug)]
pub struct AllocatedInfo {
    /// Current ref count of the allocated block.
    pub ref_count: u32,
    /// The order of the entire allocated block.
    pub order: u8,
}

/// Holds metadata for a page that is part of an allocated block but is not the head.
/// It simply points back to the head of the block.
#[derive(Clone, Copy, Debug)]
pub struct TailInfo {
    pub head: PageFrame,
}

#[derive(Debug, Clone)]
pub enum FrameState {
    /// The frame has not yet been processed by the allocator's init function.
    Uninitialized,
    /// The frame is the head of a free block of a certain order.
    Free { order: u8 },
    /// The frame is the head of an allocated block.
    AllocatedHead(AllocatedInfo),
    /// The frame is a tail page of an allocated block.
    AllocatedTail(TailInfo),
    /// The frame is being used by the slab allocator.
    Slab(Slab),
    /// The frame is part of the kernel's own image.
    Kernel,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub state: FrameState,
    // used in free nodes list (FA), Free and Partial Lists (SA).
    pub link: LinkedListLink,
    pub pfn: PageFrame,
}

intrusive_adapter!(pub FrameAdapter = UnsafeRef<Frame>: Frame { link => LinkedListLink });

impl Frame {
    pub fn new(pfn: PageFrame) -> Self {
        Self {
            state: FrameState::Uninitialized,
            link: LinkedListLink::new(),
            pfn,
        }
    }
}

#[derive(Clone)]
pub struct FrameList {
    base: *mut Frame,
    base_page: PageFrame,
    total_pages: usize,
}

impl FrameList {
    /// Initalise the FrameList
    ///
    /// # SAFETY
    ///
    /// The memory pointed to by `pages` must have been initialized to default
    /// values and be long enough to account for all pages in the system.
    pub(super) unsafe fn new(pages: &mut [Frame], base_page: PageFrame) -> Self {
        Self {
            base: pages.as_mut_ptr(),
            total_pages: pages.len(),
            base_page,
        }
    }

    pub fn base_page(&self) -> PageFrame {
        self.base_page
    }

    pub fn total_pages(&self) -> usize {
        self.total_pages
    }

    #[inline]
    fn pfn_to_index(&self, pfn: PageFrame) -> usize {
        assert!(pfn.value() >= self.base_page.value(), "PFN is below base");
        let offset = pfn.value() - self.base_page.value();
        assert!(offset < self.total_pages, "PFN is outside managed range");
        offset
    }

    pub fn get_frame(&self, pfn: PageFrame) -> *mut Frame {
        // SAFETY: There is bounds checking within the `pfn_to_index` function.
        unsafe { self.base.add(self.pfn_to_index(pfn)) }
    }
}
