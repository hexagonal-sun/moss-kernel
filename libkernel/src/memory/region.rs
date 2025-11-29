//! `region` module: Contiguous memory regions.
//!
//! This module defines `MemoryRegion<T>`, a generic abstraction for handling
//! ranges of memory in both physical and virtual address spaces.
//!
//! It provides utility methods for checking alignment, computing bounds,
//! checking containment and overlap, and mapping between physical and virtual
//! memory spaces using an `AddressTranslator`.
//!
//! ## Key Types
//! - `MemoryRegion<T>`: Generic over `MemKind` (either `Physical` or `Virtual`).
//! - `PhysMemoryRegion`: A physical memory region.
//! - `VirtMemoryRegion`: A virtual memory region.
//!
//! ## Common Operations
//! - Construction via start address + size or start + end.
//! - Checking containment and overlap of regions.
//! - Page-based offset shifting with `add_pages`.
//! - Mapping between address spaces using an `AddressTranslator`.
//!
//! ## Example
//! ```rust
//! use libkernel::memory::{address::*, region::*};
//!
//! let pa = PA::from_value(0x1000);
//! let region = PhysMemoryRegion::new(pa, 0x2000);
//! assert!(region.contains_address(PA::from_value(0x1FFF)));
//!
//! let mapped = region.map_via::<IdentityTranslator>();
//! ```

use crate::memory::PAGE_MASK;

use super::{
    PAGE_SHIFT, PAGE_SIZE,
    address::{Address, AddressTranslator, MemKind, Physical, User, Virtual},
    page::PageFrame,
};

/// A contiguous memory region of a specific memory kind (e.g., physical or virtual).
///
/// The `T` parameter is either `Physical` or `Virtual`, enforcing type safety
/// between address spaces.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct MemoryRegion<T: MemKind> {
    address: Address<T, ()>,
    size: usize,
}

impl<T: MemKind> MemoryRegion<T> {
    /// Create a new memory region from a start address and a size in bytes.
    pub const fn new(address: Address<T, ()>, size: usize) -> Self {
        Self { address, size }
    }

    /// Create an empty region with a size of 0 and address 0.
    pub const fn empty() -> Self {
        Self {
            address: Address::from_value(0),
            size: 0,
        }
    }

    /// Create a memory region from a start and end address.
    ///
    /// The size is calculated as `end - start`. No alignment is enforced.
    pub fn from_start_end_address(start: Address<T, ()>, end: Address<T, ()>) -> Self {
        assert!(end >= start);

        Self {
            address: start,
            size: (end.value() - start.value()),
        }
    }

    /// Return a new region with the same size but a different start address.
    pub fn with_start_address(mut self, new_start: Address<T, ()>) -> Self {
        self.address = new_start;
        self
    }

    /// Return the starting address of the region.
    pub const fn start_address(self) -> Address<T, ()> {
        self.address
    }

    /// Return the size of the region in bytes.
    pub const fn size(self) -> usize {
        self.size
    }

    /// Return the end address (exclusive) of the region.
    pub const fn end_address(self) -> Address<T, ()> {
        Address::from_value(self.address.value() + self.size)
    }

    /// Return the end address (inclusive) of the region.
    pub const fn end_address_inclusive(self) -> Address<T, ()> {
        Address::from_value(self.address.value() + self.size.saturating_sub(1))
    }

    /// Returns `true` if the start address is page-aligned.
    pub fn is_page_aligned(self) -> bool {
        self.address.is_page_aligned()
    }

    /// Return a new region with the given size, keeping the same start address.
    pub fn with_size(self, size: usize) -> Self {
        Self {
            address: self.address,
            size,
        }
    }

    /// Returns `true` if this region overlaps with `other`.
    ///
    /// Overlap means any portion of the address range intersects.
    pub fn overlaps(self, other: Self) -> bool {
        let start1 = self.start_address().value();
        let end1 = self.end_address().value();
        let start2 = other.start_address().value();
        let end2 = other.end_address().value();

        !(end1 <= start2 || end2 <= start1)
    }

    /// Returns `true` if this region lies before `other`.
    ///
    /// If the regions are adjacent, this function returns `true`. If the
    /// regions overlap, this functions returns `false`.
    pub fn is_before(self, other: Self) -> bool {
        self.end_address() <= other.start_address()
    }

    /// Returns `true` if this region lies after `other`.
    ///
    /// If the regions are adjacent, this function returns `true`. If the
    /// regions overlap, this functions returns `false`.
    pub fn is_after(self, other: Self) -> bool {
        self.start_address() >= other.end_address()
    }

    /// Try to merge this region with another.
    ///
    /// If the regions overlap or are contiguous, returns a new merged region.
    /// Otherwise, returns `None`.
    pub fn merge(self, other: Self) -> Option<Self> {
        let start1 = self.address;
        let end1 = self.end_address();
        let start2 = other.address;
        let end2 = other.end_address();

        if end1 >= start2 && start1 <= end2 {
            let merged_start = core::cmp::min(start1, start2);
            let merged_end = core::cmp::max(end1, end2);

            Some(Self {
                address: merged_start,
                size: merged_end.value() - merged_start.value(),
            })
        } else {
            None
        }
    }

    /// Punches a hole (`other`) into the region.
    ///
    /// # Returns
    /// * `(None, None)` if `other` fully contains `self`.
    /// * `(Some(Left), None)` if `other` overlaps the tail (end) of `self`,
    ///   or if `other` is strictly after `self` (no overlap).
    /// * `(None, Some(Right))` if `other` overlaps the head (start) of `self`,
    ///   or if `other` is strictly before `self` (no overlap).
    /// * `(Some(Left), Some(Right))` if `other` is strictly inside `self`.
    pub fn punch_hole(self, other: Self) -> (Option<Self>, Option<Self>) {
        let s_start = self.start_address();
        let s_end = self.end_address();
        let o_start = other.start_address();
        let o_end = other.end_address();

        // Calculate the "Left" remainder:
        // Exists if self starts before other starts.
        // The region is [self.start, min(self.end, other.start)).
        let left = if s_start < o_start {
            let end = core::cmp::min(s_end, o_start);
            if end > s_start {
                Some(Self::from_start_end_address(s_start, end))
            } else {
                None
            }
        } else {
            None
        };

        // Calculate the "Right" remainder:
        // Exists if self ends after other ends.
        // The region is [max(self.start, other.end), self.end).
        let right = if s_end > o_end {
            let start = core::cmp::max(s_start, o_end);
            if start < s_end {
                Some(Self::from_start_end_address(start, s_end))
            } else {
                None
            }
        } else {
            None
        };

        (left, right)
    }

    /// Returns `true` if this region fully contains `other`.
    pub fn contains(self, other: Self) -> bool {
        self.start_address().value() <= other.start_address().value()
            && self.end_address().value() >= other.end_address().value()
    }

    /// Returns `true` if this region contains the given address.
    pub fn contains_address(self, addr: Address<T, ()>) -> bool {
        let val = addr.value();
        val >= self.start_address().value() && val < self.end_address().value()
    }

    /// Shift the region forward by `n` pages.
    ///
    /// Decreases the size by the corresponding number of bytes.
    /// Will saturate at zero if size underflows.
    #[must_use]
    pub fn add_pages(self, n: usize) -> Self {
        let offset: Address<T, ()> = Address::from_value(n << PAGE_SHIFT);
        Self {
            address: Address::from_value(self.address.value() + offset.value()),
            size: self.size.saturating_sub(offset.value()),
        }
    }

    /// Converts this region into a `MappableRegion`.
    ///
    /// This calculates the smallest page-aligned region that fully contains the
    /// current region, and captures the original start address's offset from
    /// the new aligned start.
    pub fn to_mappable_region(self) -> MappableRegion<T> {
        let aligned_start_addr = self.start_address().align(PAGE_SIZE);
        let aligned_end_addr = self.end_address().align_up(PAGE_SIZE);

        let aligned_region =
            MemoryRegion::from_start_end_address(aligned_start_addr, aligned_end_addr);

        MappableRegion {
            region: aligned_region,
            offset_from_page_start: self.start_address().page_offset(),
        }
    }

    /// Increases the capacity of the region by size bytes.
    pub(crate) fn expand_by(&mut self, size: usize) {
        assert!(size & PAGE_MASK == 0);

        self.size += size;
    }

    /// Calculates the common overlapping region between `self` and `other`.
    ///
    /// If the regions overlap, this returns a `Some(MemoryRegion)` representing
    /// the shared intersection. If they are merely adjacent or do not overlap at
    /// all, this returns `None`.
    ///
    /// Visually, if region `A = [ A_start ... A_end )` and
    /// region `B = [ B_start ... B_end )`, the intersection is
    /// `[ max(A_start, B_start) ... min(A_end, B_end) )`.
    ///
    /// # Example
    ///
    /// ```
    /// use libkernel::memory::region::VirtMemoryRegion;
    /// use libkernel::memory::address::VA;
    /// let region1 = VirtMemoryRegion::new(VA::from_value(0x1000), 0x2000); // Range: [0x1000, 0x3000)
    /// let region2 = VirtMemoryRegion::new(VA::from_value(0x2000), 0x2000); // Range: [0x2000, 0x4000)
    ///
    /// let intersection = region1.intersection(region2).unwrap();
    ///
    /// assert_eq!(intersection.start_address().value(), 0x2000);
    /// assert_eq!(intersection.end_address().value(), 0x3000);
    /// assert_eq!(intersection.size(), 0x1000);
    /// ```
    pub fn intersection(self, other: Self) -> Option<Self> {
        // Determine the latest start address and the earliest end address.
        let intersection_start = core::cmp::max(self.start_address(), other.start_address());
        let intersection_end = core::cmp::min(self.end_address(), other.end_address());

        // A valid, non-empty overlap exists only if the start of the
        // potential intersection is before its end.
        if intersection_start < intersection_end {
            Some(Self::from_start_end_address(
                intersection_start,
                intersection_end,
            ))
        } else {
            None
        }
    }

    /// Returns a new region that is page-aligned and fully contains the
    /// original.
    ///
    /// This is achieved by rounding the region's start address down to the
    /// nearest page boundary and rounding the region's end address up to the
    /// nearest page boundary.
    ///
    /// # Example
    /// ```
    /// use libkernel::memory::region::VirtMemoryRegion;
    /// use libkernel::memory::address::VA;
    /// let region = VirtMemoryRegion::new(VA::from_value(0x1050), 0x1F00);
    /// // Original region: [0x1050, 0x2F50)
    ///
    /// let aligned_region = region.align_to_page_boundary();
    ///
    /// // Aligned region: [0x1000, 0x3000)
    /// assert_eq!(aligned_region.start_address().value(), 0x1000);
    /// assert_eq!(aligned_region.end_address().value(), 0x3000);
    /// assert_eq!(aligned_region.size(), 0x2000);
    /// ```
    #[must_use]
    pub fn align_to_page_boundary(self) -> Self {
        let aligned_start = self.start_address().align(PAGE_SIZE);

        let aligned_end = self.end_address().align_up(PAGE_SIZE);

        Self::from_start_end_address(aligned_start, aligned_end)
    }

    /// Returns an iterator that yields the starting address of each 4KiB page
    /// contained within the memory region.
    ///
    /// The iterator starts at the region's base address and advances in 4KiB
    /// (`PAGE_SIZE`) increments. If the region's size is not a perfect multiple
    /// of `PAGE_SIZE`, any trailing memory fragment at the end of the region
    /// that does not constitute a full page is ignored.
    ///
    /// A region with a size of zero will produce an empty iterator.
    ///
    /// # Examples
    ///
    /// ```
    /// use libkernel::memory::PAGE_SIZE;
    /// use libkernel::memory::address::VA;
    /// use libkernel::memory::region::VirtMemoryRegion;
    /// let start_va = VA::from_value(0x10000);
    /// // A region covering 2.5 pages of memory.
    /// let region = VirtMemoryRegion::new(start_va, 2 * PAGE_SIZE + 100);
    ///
    /// let pages: Vec<VA> = region.iter_pages().collect();
    ///
    /// // The iterator should yield addresses for the two full pages.
    /// assert_eq!(pages.len(), 2);
    /// assert_eq!(pages[0], VA::from_value(0x10000));
    /// assert_eq!(pages[1], VA::from_value(0x11000));
    /// ```
    pub fn iter_pages(self) -> impl Iterator<Item = Address<T, ()>> {
        let mut count = 0;
        let pages_count = self.size >> PAGE_SHIFT;

        core::iter::from_fn(move || {
            let addr = self.start_address().add_pages(count);

            if count < pages_count {
                count += 1;
                Some(addr)
            } else {
                None
            }
        })
    }

    pub fn iter_pfns(self) -> impl Iterator<Item = PageFrame> {
        let mut count = 0;
        let pages_count = self.size >> PAGE_SHIFT;
        let start = self.start_address().to_pfn();

        core::iter::from_fn(move || {
            let pfn = PageFrame::from_pfn(start.value() + count);

            if count < pages_count {
                count += 1;
                Some(pfn)
            } else {
                None
            }
        })
    }
}

/// A memory region in physical address space.
pub type PhysMemoryRegion = MemoryRegion<Physical>;

impl PhysMemoryRegion {
    /// Map the physical region to virtual space using a translator.
    pub fn map_via<T: AddressTranslator<()>>(self) -> VirtMemoryRegion {
        VirtMemoryRegion::new(self.address.to_va::<T>(), self.size)
    }
}

/// A memory region in virtual address space.
pub type VirtMemoryRegion = MemoryRegion<Virtual>;

impl VirtMemoryRegion {
    /// Map the virtual region to physical space using a translator.
    pub fn map_via<T: AddressTranslator<()>>(self) -> PhysMemoryRegion {
        PhysMemoryRegion::new(self.address.to_pa::<T>(), self.size)
    }
}

/// A memory region of user-space addresses.
pub type UserMemoryRegion = MemoryRegion<User>;

/// A representation of a `MemoryRegion` that has been expanded to be page-aligned.
///
/// This struct holds the new, larger, page-aligned region, as well as the
/// byte offset of the original region's start address from the start of the
/// new aligned region. This is essential for MMU operations that must map
//  full pages but return a pointer to an unaligned location within that page.
#[derive(Copy, Clone)]
pub struct MappableRegion<T: MemKind> {
    /// The fully-encompassing, page-aligned memory region.
    region: MemoryRegion<T>,
    /// The byte offset of the original region's start from the aligned region's start.
    offset_from_page_start: usize,
}

impl<T: MemKind> MappableRegion<T> {
    /// Returns the full, page-aligned region that is suitable for mapping.
    pub const fn region(&self) -> MemoryRegion<T> {
        self.region
    }

    /// Returns the offset of the original data within the aligned region.
    pub const fn offset(&self) -> usize {
        self.offset_from_page_start
    }
}

#[cfg(test)]
mod tests {
    use super::PhysMemoryRegion;
    use crate::memory::{
        PAGE_SIZE,
        address::{PA, VA},
        region::VirtMemoryRegion,
    };

    fn region(start: usize, size: usize) -> PhysMemoryRegion {
        PhysMemoryRegion::new(PA::from_value(start), size)
    }

    #[test]
    fn merge_adjacent() {
        let a = region(0x100, 0x10);
        let b = region(0x110, 0x10);
        let merged = a.merge(b).unwrap();

        assert_eq!(merged.address.value(), 0x100);
        assert_eq!(merged.size(), 0x20);
    }

    #[test]
    fn merge_overlap() {
        let a = region(0x100, 0x20);
        let b = region(0x110, 0x20);
        let merged = a.merge(b).unwrap();

        assert_eq!(merged.address.value(), 0x100);
        assert_eq!(merged.size(), 0x30);
    }

    #[test]
    fn merge_identical() {
        let a = region(0x100, 0x20);
        let b = region(0x100, 0x20);
        let merged = a.merge(b).unwrap();

        assert_eq!(merged.address.value(), 0x100);
        assert_eq!(merged.size(), 0x20);
    }

    #[test]
    fn merge_non_touching() {
        let a = region(0x100, 0x10);
        let b = region(0x200, 0x10);
        assert!(a.merge(b).is_none());
    }

    #[test]
    fn merge_reverse_order() {
        let a = region(0x200, 0x20);
        let b = region(0x100, 0x100);
        let merged = a.merge(b).unwrap();

        assert_eq!(merged.address.value(), 0x100);
        assert_eq!(merged.size(), 0x120);
    }

    #[test]
    fn merge_partial_overlap() {
        let a = region(0x100, 0x30); // [0x100, 0x130)
        let b = region(0x120, 0x20); // [0x120, 0x140)
        let merged = a.merge(b).unwrap(); // should be [0x100, 0x140)

        assert_eq!(merged.address.value(), 0x100);
        assert_eq!(merged.size(), 0x40);
    }

    #[test]
    fn test_contains_region() {
        let a = region(0x1000, 0x300);
        let b = region(0x1100, 0x100);
        assert!(a.contains(b));
        assert!(!b.contains(a));
    }

    #[test]
    fn test_contains_address() {
        let region = region(0x1000, 0x100);
        assert!(region.contains_address(PA::from_value(0x1000)));
        assert!(region.contains_address(PA::from_value(0x10FF)));
        assert!(!region.contains_address(PA::from_value(0x1100)));
    }

    #[test]
    fn test_iter_pages_exact_multiple() {
        let start_va = VA::from_value(0x10000);
        let num_pages = 3;
        let region = VirtMemoryRegion::new(start_va, num_pages * PAGE_SIZE);

        let pages: Vec<VA> = region.iter_pages().collect();

        assert_eq!(pages.len(), num_pages);

        assert_eq!(pages[0], VA::from_value(0x10000));
        assert_eq!(pages[1], VA::from_value(0x11000));
        assert_eq!(pages[2], VA::from_value(0x12000));
    }

    #[test]
    fn test_iter_pages_single_page() {
        let start_va = VA::from_value(0x20000);
        let region = VirtMemoryRegion::new(start_va, PAGE_SIZE);

        let mut iter = region.iter_pages();

        assert_eq!(iter.next(), Some(start_va));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_iter_pages_empty_region() {
        let start_va = VA::from_value(0x30000);

        let region = VirtMemoryRegion::new(start_va, 0);

        assert_eq!(region.iter_pages().count(), 0);
    }

    #[test]
    fn test_iter_pages_size_not_a_multiple_of_page_size() {
        let start_va = VA::from_value(0x40000);
        let num_pages = 5;
        let size = num_pages * PAGE_SIZE + 123;
        let region = VirtMemoryRegion::new(start_va, size);

        assert_eq!(region.iter_pages().count(), num_pages);

        assert_eq!(
            region.iter_pages().last(),
            Some(start_va.add_pages(num_pages - 1))
        );
    }

    #[test]
    fn test_iter_pages_large_count() {
        let start_va = VA::from_value(0x5_0000_0000);
        let num_pages = 1000;
        let region = VirtMemoryRegion::new(start_va, num_pages * PAGE_SIZE);

        assert_eq!(region.iter_pages().count(), num_pages);
    }

    #[test]
    fn test_punch_hole_middle() {
        // Region: [0x1000 ... 0x5000) (size 0x4000)
        // Hole:   [0x2000 ... 0x3000)
        // Expect: Left [0x1000, 0x2000), Right [0x3000, 0x5000)
        let main = region(0x1000, 0x4000);
        let hole = region(0x2000, 0x1000);

        let (left, right) = main.punch_hole(hole);

        assert_eq!(left, Some(region(0x1000, 0x1000)));
        assert_eq!(right, Some(region(0x3000, 0x2000)));
    }

    #[test]
    fn test_punch_hole_consumes_start() {
        // Region: [0x2000 ... 0x4000)
        // Hole:   [0x1000 ... 0x3000) (Overlaps the beginning)
        // Expect: Left None, Right [0x3000, 0x4000)
        let main = region(0x2000, 0x2000);
        let hole = region(0x1000, 0x2000);

        let (left, right) = main.punch_hole(hole);

        assert_eq!(left, None);
        assert_eq!(right, Some(region(0x3000, 0x1000)));
    }

    #[test]
    fn test_punch_hole_consumes_end() {
        // Region: [0x1000 ... 0x3000)
        // Hole:   [0x2000 ... 0x4000) (Overlaps the end)
        // Expect: Left [0x1000, 0x2000), Right None
        let main = region(0x1000, 0x2000);
        let hole = region(0x2000, 0x2000);

        let (left, right) = main.punch_hole(hole);

        assert_eq!(left, Some(region(0x1000, 0x1000)));
        assert_eq!(right, None);
    }

    #[test]
    fn test_punch_hole_exact_match() {
        // Region: [0x1000 ... 0x2000)
        // Hole:   [0x1000 ... 0x2000)
        // Expect: None, None
        let main = region(0x1000, 0x1000);
        let hole = region(0x1000, 0x1000);

        let (left, right) = main.punch_hole(hole);

        assert_eq!(left, None);
        assert_eq!(right, None);
    }

    #[test]
    fn test_punch_hole_fully_contained() {
        // Region: [0x2000 ... 0x3000)
        // Hole:   [0x1000 ... 0x4000) (Swallows the region entirely)
        // Expect: None, None
        let main = region(0x2000, 0x1000);
        let hole = region(0x1000, 0x3000);

        let (left, right) = main.punch_hole(hole);

        assert_eq!(left, None);
        assert_eq!(right, None);
    }

    #[test]
    fn test_punch_hole_touching_edges() {
        // Case A: Hole ends exactly where region starts
        // Region: [0x2000 ... 0x3000)
        // Hole:   [0x1000 ... 0x2000)
        // Result: Region is to the right of the hole.
        let main = region(0x2000, 0x1000);
        let hole = region(0x1000, 0x1000);
        assert_eq!(main.punch_hole(hole), (None, Some(main)));

        // Case B: Hole starts exactly where region ends
        // Region: [0x1000 ... 0x2000)
        // Hole:   [0x2000 ... 0x3000)
        // Result: Region is to the left of the hole.
        let main = region(0x1000, 0x1000);
        let hole = region(0x2000, 0x1000);
        assert_eq!(main.punch_hole(hole), (Some(main), None));
    }

    #[test]
    fn test_punch_hole_disjoint() {
        // Hole is completely far away (after)
        // Region: [0x1000 ... 0x2000)
        // Hole:   [0x5000 ... 0x6000)
        // Result: Region is to the left of the hole.
        let main = region(0x1000, 0x1000);
        let hole = region(0x5000, 0x1000);
        assert_eq!(main.punch_hole(hole), (Some(main), None));

        // Hole is completely far away (before)
        // Region: [0x5000 ... 0x6000)
        // Hole:   [0x1000 ... 0x2000)
        // Result: Region is to the right of the hole.
        let main = region(0x5000, 0x1000);
        let hole = region(0x1000, 0x1000);
        assert_eq!(main.punch_hole(hole), (None, Some(main)));
    }
}
