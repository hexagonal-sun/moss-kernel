//! `smalloc` module: A simple physical memory allocator for early boot and
//! kernel use.
//!
//! This allocator manages a fixed number of memory and reservation regions and
//! supports basic allocation and freeing of physical memory blocks. //!
//!
//! ## Key Components
//! - [`RegionList`]: A growable list of non-overlapping [`PhysMemoryRegion`]s,
//!   allowing for merging, inserting, and iteration.
//! - [`Smalloc`]: A memory allocator that manages physical memory and reserved
//!   regions, ensuring that allocated blocks do not overlap reservations.
//!
//! ## Key Features
//! - Alignment-aware allocation via `Smalloc::alloc()`.
//! - Safe freeing of physical memory blocks.
//! - Memory and reservation region tracking with automatic merging and
//!   splitting.
//! - Optional region list growth via `permit_region_list_reallocs()`.
//!
//! ## Safety
//! - All raw pointer manipulations are internal and guarded.
//! - Growth of region lists is marked `unsafe` to ensure critical reservations
//!   are made before potentially reallocating region list memory.
use core::{
    iter::Peekable,
    marker::PhantomData,
    ops::{Index, IndexMut},
};

use crate::error::{KernelError, Result};

use super::{
    PAGE_SHIFT, PAGE_SIZE,
    address::{AddressTranslator, PA},
    page::PageFrame,
    region::PhysMemoryRegion,
};

#[derive(Clone)]
pub struct RegionList {
    count: usize,
    max: usize,
    regions: *mut PhysMemoryRegion,
}

impl RegionList {
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub const fn new(max: usize, region_ptr: *mut PhysMemoryRegion) -> Self {
        Self {
            count: 0,
            max,
            regions: region_ptr,
        }
    }

    fn remove_at(&mut self, index: usize) {
        if index >= self.count {
            return;
        }

        unsafe {
            self.regions
                .add(index)
                .copy_from(self.regions.add(index + 1), self.count - index - 1);
        }

        self.count -= 1;
    }

    pub fn insert_region(&mut self, mut new_region: PhysMemoryRegion) -> Result<()> {
        if self.count == self.max {
            return Err(KernelError::NoMemory);
        }

        // Step 1: Try to merge with previous and next regions
        let mut i = 0;

        while i < self.count {
            let existing = self[i];
            if let Some(merged) = existing.merge(new_region) {
                new_region = merged;

                // Remove the old region
                self.remove_at(i);
                // Restart scan to check for new overlaps (merging can expand the region)
                i = 0;
            } else {
                i += 1;
            }
        }

        // Step 2: Find the correct insert position.
        let insert_idx = self
            .iter()
            .position(|x| x.start_address() > new_region.start_address())
            .unwrap_or(self.count);

        // Step 3: Shift elements to make room
        unsafe {
            self.regions
                .add(insert_idx + 1)
                .copy_from(self.regions.add(insert_idx), self.count - insert_idx);
        }

        // Step 4: Insert the region
        self.count += 1;
        self[insert_idx] = new_region;
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = PhysMemoryRegion> {
        (0..self.count).map(|x| self[x])
    }

    fn requires_reallocation(&self) -> bool {
        // Check whether the reserved list needs to be expanded. The worst
        // case here is we free in the middle of a merged region.  For example:
        //
        // res (0x100 - 0x200)
        // free (0x120 - 0x180)
        //
        // Then we would need an *extra* slot in the reserved list for the
        // two fragments:
        //
        // res(0x100 - 0x120)
        // res(0x180 - 0x200).
        self.count >= self.max - 1
    }
}

impl Index<usize> for RegionList {
    type Output = PhysMemoryRegion;

    fn index(&self, index: usize) -> &Self::Output {
        if index >= self.count {
            panic!(
                "Smalloc region index {} > region count {}",
                index, self.count
            );
        }

        unsafe { &(*self.regions.add(index)) }
    }
}

impl IndexMut<usize> for RegionList {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        if index >= self.count {
            panic!(
                "Smalloc region index {} > region count {}",
                index, self.count
            );
        }

        unsafe { &mut (*self.regions.add(index)) }
    }
}

pub struct Smalloc<T: AddressTranslator<()>> {
    pub memory: RegionList,
    pub res: RegionList,
    permit_region_realloc: bool,
    _phantom: PhantomData<T>,
}

enum RegionListType {
    Mem,
    Res,
}

unsafe impl<T: AddressTranslator<()>> Send for Smalloc<T> {}

impl<T: AddressTranslator<()>> Smalloc<T> {
    pub const fn new(memory: RegionList, reserved_list: RegionList) -> Self {
        Self {
            memory,
            res: reserved_list,
            permit_region_realloc: false,
            _phantom: PhantomData,
        }
    }

    /// Allows the allocator to reallocate the region lists if they grow beyond `max`.
    ///
    /// # Safety
    ///
    /// This function is marked `unsafe` for several important reasons:
    ///
    /// - Memory has been added to the `memory` list.
    /// - All necessary reservations must be made *before* enabling reallocation.
    ///   For example, to ensure that critical areas like kernel text are not
    ///   accidentally overwritten.
    /// - The memory system must be initialized enough that allocated physical
    ///   addresses (PAs) can be safely converted into valid virtual addresses (VAs).
    pub unsafe fn permit_region_list_reallocs(&mut self) {
        self.permit_region_realloc = true;
    }

    fn find_allocation_location(&self, size: usize, align: usize) -> Option<PA> {
        let mut res_idx = 0;

        for mem_region in self.memory.iter() {
            let mut candidate =
                PhysMemoryRegion::new(mem_region.start_address().align_up(align), size);

            while mem_region.contains(candidate) {
                // Skip over reserved regions that end before candidate.
                while res_idx < self.res.count
                    && self.res[res_idx].end_address().value() <= candidate.start_address().value()
                {
                    res_idx += 1;
                }

                // If we're overlapping a reserved region, move candidate past
                // it
                if res_idx < self.res.count {
                    let res_region = self.res[res_idx];
                    if candidate.overlaps(res_region) {
                        // move candidate just after this reservation, realigned
                        candidate =
                            candidate.with_start_address(res_region.end_address().align_up(align));
                        continue;
                    }
                }

                return Some(candidate.start_address());
            }
        }

        None
    }

    pub fn alloc(&mut self, size: usize, align: usize) -> Result<PA> {
        if self.res.requires_reallocation() {
            self.grow_region_list(RegionListType::Res)?;
        }

        let address = self
            .find_allocation_location(size, align)
            .ok_or(KernelError::NoMemory)?;

        // Allocation fits and doesn't overlap any reservation
        self.res.insert_region(PhysMemoryRegion::new(address, size))?;

        Ok(address)
    }

    pub fn free(&mut self, addr: PA, size: usize) -> Result<()> {
        let region_to_remove = PhysMemoryRegion::new(addr, size);

        let index_opt = self.res.iter().position(|r| r.contains(region_to_remove));

        if let Some(index) = index_opt {
            let old = self.res[index];

            // We *may* grow the reserved list with a split here. See comment
            // above `grow_region_list` for details.
            if self.res.requires_reallocation() {
                self.grow_region_list(RegionListType::Res)?;
            }

            // Shift tail down first (we’ll reinstate fragments after)
            unsafe {
                self.res
                    .regions
                    .add(index)
                    .copy_from(self.res.regions.add(index + 1), self.res.count - index - 1);
            }

            self.res.count -= 1;

            // Compute potential fragments
            let start_val = old.start_address().value();
            let end_val = old.end_address().value();

            let remove_start = region_to_remove.start_address().value();
            let remove_end = region_to_remove.end_address().value();

            // Left fragment (before freed block)
            if remove_start > start_val {
                let left =
                    PhysMemoryRegion::new(PA::from_value(start_val), remove_start - start_val);
                self.res.insert_region(left)?;
            }

            // Right fragment (after freed block)
            if remove_end < end_val {
                let right = PhysMemoryRegion::new(PA::from_value(remove_end), end_val - remove_end);
                self.res.insert_region(right)?;
            }

            Ok(())
        } else {
            Err(KernelError::NoMemRegion)
        }
    }

    fn grow_region_list(&mut self, list_type: RegionListType) -> Result<()> {
        if !self.permit_region_realloc {
            return Err(KernelError::NoMemory);
        }

        let mut list = match list_type {
            RegionListType::Mem => self.memory.clone(),
            RegionListType::Res => self.res.clone(),
        };

        let old_max = list.max;
        let new_max = old_max * 2;
        let new_size = new_max * core::mem::size_of::<PhysMemoryRegion>();
        let align = core::mem::align_of::<PhysMemoryRegion>();

        if let Some(addr) = self.find_allocation_location(new_size, align) {
            let va = addr.to_va::<T>();
            let new_ptr: *mut PhysMemoryRegion = va.as_ptr_mut().cast();

            unsafe {
                new_ptr.copy_from(list.regions, list.count);
            }

            let old_size = old_max * core::mem::size_of::<PhysMemoryRegion>();
            let old_ptr = PA::from_value(list.regions.expose_provenance());

            list.regions = new_ptr;
            list.max = new_max;

            match list_type {
                RegionListType::Mem => self.memory = list,
                RegionListType::Res => self.res = list,
            }

            // Free old buffer
            let _ = self.free(old_ptr, old_size);

            // Allocate the new buffer.
            self.res.insert_region(PhysMemoryRegion::new(
                PA::from_value(new_ptr.expose_provenance()),
                new_size,
            ))?;

            Ok(())
        } else {
            Err(KernelError::NoMemory)
        }
    }

    pub fn add_reservation(&mut self, region: PhysMemoryRegion) -> Result<()> {
        if self.res.requires_reallocation() {
            self.grow_region_list(RegionListType::Res)?;
        }

        self.res.insert_region(region)?;

        Ok(())
    }

    pub fn base_ram_base_address(&self) -> Option<PA> {
        if self.memory.is_empty() {
            None
        } else {
            Some(self.memory[0].start_address())
        }
    }

    pub fn add_memory(&mut self, region: PhysMemoryRegion) -> Result<()> {
        if self.memory.requires_reallocation() {
            self.grow_region_list(RegionListType::Mem)?;
        }

        self.memory.insert_region(region)?;

        Ok(())
    }

    pub fn alloc_page(&mut self) -> Result<PageFrame> {
        let pa = self.alloc(PAGE_SIZE, PAGE_SIZE)?;

        Ok(PageFrame::from_pfn(pa.value() >> PAGE_SHIFT))
    }

    pub fn iter_memory(&self) -> impl Iterator<Item = PhysMemoryRegion> {
        self.memory.iter()
    }

    pub fn get_memory_list(&self) -> RegionList {
        self.memory.clone()
    }

    /// Returns an iterator over all free memory regions.
    ///
    /// This iterator walks through all available memory regions and subtracts
    /// any reserved regions, yielding the resulting free chunks.
    pub fn iter_free(&self) -> impl Iterator<Item = PhysMemoryRegion> {
        FreeRegionsIter {
            mem_iter: self.iter_memory(),
            res_iter: self.res.iter().peekable(),
            current_fragment: None,
        }
    }
}

pub struct FreeRegionsIter<M, R>
where
    M: Iterator<Item = PhysMemoryRegion>,
    R: Iterator<Item = PhysMemoryRegion>,
{
    mem_iter: M,
    res_iter: Peekable<R>,
    current_fragment: Option<PhysMemoryRegion>,
}

impl<M, R> Iterator for FreeRegionsIter<M, R>
where
    M: Iterator<Item = PhysMemoryRegion>,
    R: Iterator<Item = PhysMemoryRegion>,
{
    type Item = PhysMemoryRegion;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let mut fragment = if let Some(frag) = self.current_fragment.take() {
                frag
            } else {
                // If the main memory iterator is exhausted, we are done.
                self.mem_iter.next()?
            };

            // Advance the reservation iterator past any reservations that are
            // entirely before our current fragment.
            while let Some(res) = self.res_iter.peek() {
                if res.is_before(fragment) {
                    // This reservation is irrelevant to this and all future fragments.
                    self.res_iter.next(); // Consume it.
                } else {
                    break;
                }
            }

            // Check the next reservation.
            let res_region = match self.res_iter.peek() {
                // If there are no more reservations, this fragment and all
                // subsequent memory regions are entirely free.
                None => return Some(fragment),
                Some(res) => res,
            };

            // If the next reservation is entirely after our fragment, then our
            // fragment must be completely free.
            if res_region.is_after(fragment) {
                return Some(fragment);
            }

            // Check the fragment has a free portion *before* the reservation.
            if fragment.start_address() < res_region.start_address() {
                let free_prefix = PhysMemoryRegion::from_start_end_address(
                    fragment.start_address(),
                    res_region.start_address(),
                );

                // The remainder of the fragment might exist *after* the reservation.
                // If so, save it in `current_fragment` to be processed in the next
                // iteration of the `loop`.
                if res_region.end_address() < fragment.end_address() {
                    self.current_fragment = Some(PhysMemoryRegion::from_start_end_address(
                        res_region.end_address(),
                        fragment.end_address(),
                    ));
                }

                // Consume this reservation.
                self.res_iter.next();

                return Some(free_prefix);
            }
            // Check the fragment starts at or inside the reservation.
            else {
                // The reservation eats the beginning of our fragment.
                let new_start = res_region.end_address();

                // Consomue this reservation.
                self.res_iter.next();

                if new_start >= fragment.end_address() {
                    // The reservation consumed the entire fragment.
                    continue;
                } else {
                    // A smaller piece of the fragment remains. Update it and
                    // re-run the loop to check it against the *next*
                    // reservation.
                    fragment =
                        PhysMemoryRegion::from_start_end_address(new_start, fragment.end_address());

                    self.current_fragment = Some(fragment);

                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::{marker::PhantomData, mem::MaybeUninit};

    use rand::{Rng, SeedableRng};

    use crate::{
        error::KernelError,
        memory::{
            address::{IdentityTranslator, PA},
            region::PhysMemoryRegion,
        },
    };

    use super::{RegionList, Smalloc};

    fn get_smalloc() -> Smalloc<IdentityTranslator> {
        let mem_region: &mut [MaybeUninit<PhysMemoryRegion>] =
            vec![MaybeUninit::uninit(); 16].leak();
        let res_region: &mut [MaybeUninit<PhysMemoryRegion>] =
            vec![MaybeUninit::uninit(); 16].leak();

        Smalloc {
            memory: RegionList::new(16, mem_region.as_mut_ptr().cast()),
            res: RegionList::new(16, res_region.as_mut_ptr().cast()),
            permit_region_realloc: false,
            _phantom: PhantomData,
        }
    }

    /// Helper to check a region's start and size.
    fn check_region(region: &PhysMemoryRegion, start: usize, size: usize) {
        assert_eq!(
            region.start_address().value(),
            start,
            "Region start address mismatch"
        );
        assert_eq!(region.size(), size, "Region size mismatch");
    }

    #[test]
    fn add_region_perserves_order() {
        let mut smalloc = get_smalloc();

        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x700), 0x50))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x800), 0x50))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x200), 0x50))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x100), 0x50))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x770), 0x10))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x900), 0x50))
            .unwrap();

        let mut iter = smalloc.memory.iter();

        assert_eq!(iter.next().unwrap().start_address().value(), 0x100);
        assert_eq!(iter.next().unwrap().start_address().value(), 0x200);
        assert_eq!(iter.next().unwrap().start_address().value(), 0x700);
        assert_eq!(iter.next().unwrap().start_address().value(), 0x770);
        assert_eq!(iter.next().unwrap().start_address().value(), 0x800);
        assert_eq!(iter.next().unwrap().start_address().value(), 0x900);
    }

    #[test]
    fn add_region_merges_adjacent() {
        let mut smalloc = get_smalloc();

        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x100))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1100), 0x100))
            .unwrap();

        let mut iter = smalloc.memory.iter();
        let region = iter.next().unwrap();

        assert_eq!(region.start_address().value(), 0x1000);
        assert_eq!(region.size(), 0x200);
        assert!(iter.next().is_none());
    }

    #[test]
    fn add_region_merges_overlap() {
        let mut smalloc = get_smalloc();

        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x200))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1100), 0x200))
            .unwrap();

        let mut iter = smalloc.memory.iter();
        let region = iter.next().unwrap();

        assert_eq!(region.start_address().value(), 0x1000);
        assert_eq!(region.size(), 0x300); // from 0x1000 to 0x1400
        assert!(iter.next().is_none());
    }

    #[test]
    fn add_region_merges_multiple_regions() {
        let mut smalloc = get_smalloc();

        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x100))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1200), 0x100))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1100), 0x100))
            .unwrap(); // fills the gap

        let mut iter = smalloc.memory.iter();
        let region = iter.next().unwrap();

        assert_eq!(region.start_address().value(), 0x1000);
        assert_eq!(region.size(), 0x300);
        assert!(iter.next().is_none());
    }

    #[test]
    fn add_region_does_not_merge_non_adjacent() {
        let mut smalloc = get_smalloc();

        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x100))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1200), 0x100))
            .unwrap();

        let mut iter = smalloc.memory.iter();
        let region1 = iter.next().unwrap();
        let region2 = iter.next().unwrap();

        assert_eq!(region1.start_address().value(), 0x1000);
        assert_eq!(region1.size(), 0x100);
        assert_eq!(region2.start_address().value(), 0x1200);
        assert_eq!(region2.size(), 0x100);
        assert!(iter.next().is_none());
    }

    #[test]
    fn add_region_merge_order_independent() {
        let mut smalloc = get_smalloc();

        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1100), 0x100))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x100))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1200), 0x100))
            .unwrap();

        let mut iter = smalloc.memory.iter();
        let region = iter.next().unwrap();

        assert_eq!(region.start_address().value(), 0x1000);
        assert_eq!(region.size(), 0x300);
        assert!(iter.next().is_none());
    }

    #[test]
    fn alloc_basic_success() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000))
            .unwrap();

        let addr = smalloc.alloc(0x100, 0x100).unwrap();
        assert_eq!(addr.value(), 0x1000);

        let reserved = smalloc.res.iter().next().unwrap();
        assert_eq!(reserved.start_address().value(), 0x1000);
        assert_eq!(reserved.size(), 0x100);
    }

    #[test]
    fn alloc_alignment_handling() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1003), 0x1000))
            .unwrap();

        let addr = smalloc.alloc(0x100, 0x100).unwrap();
        assert_eq!(addr.value() % 0x100, 0); // aligned
        assert!(addr.value() >= 0x1003); // starts at or after base
    }

    #[test]
    fn alloc_with_reservation_gap() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000))
            .unwrap();

        // Reserve a middle region
        smalloc
            .res
            .insert_region(PhysMemoryRegion::new(PA::from_value(0x1200), 0x100))
            .unwrap();

        // Alloc before the reserved region
        let addr1 = smalloc.alloc(0x100, 0x100).unwrap();
        assert_eq!(addr1.value(), 0x1000);

        let addr2 = smalloc.alloc(0x100, 0x100).unwrap();
        assert_eq!(addr2.value(), 0x1100);

        // Alloc after the reserved region
        let addr3 = smalloc.alloc(0x100, 0x100).unwrap();
        assert!(addr3.value() >= 0x1300); // aligned up after 0x1200 + 0x100
    }

    #[test]
    fn alloc_avoids_overlap() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x200))
            .unwrap();

        // Reserve something at the start
        smalloc
            .res
            .insert_region(PhysMemoryRegion::new(PA::from_value(0x1000), 0x100))
            .unwrap();

        // Should only be able to allocate after 0x1100
        let addr = smalloc.alloc(0x80, 0x10).unwrap();
        assert!(addr.value() >= 0x1100);
        assert!(addr.value() + 0x80 <= 0x1200);
    }

    #[test]
    fn alloc_fails_when_full() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x100))
            .unwrap();

        // Fill it
        assert!(smalloc.alloc(0x80, 0x10).is_ok());
        assert!(smalloc.alloc(0x80, 0x10).is_ok());
        assert!(smalloc.alloc(0x80, 0x10).is_err()); // should be full
    }

    #[test]
    fn alloc_exact_fit() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x2000), 0x100))
            .unwrap();

        let addr = smalloc.alloc(0x100, 0x1).unwrap();
        assert_eq!(addr.value(), 0x2000);
        assert!(smalloc.alloc(0x1, 0x1).is_err()); // No space left
    }

    #[test]
    fn alloc_multiple_allocations() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x3000), 0x400))
            .unwrap();

        let a1 = smalloc.alloc(0x100, 0x100).unwrap();
        let a2 = smalloc.alloc(0x100, 0x100).unwrap();
        let a3 = smalloc.alloc(0x100, 0x100).unwrap();

        assert_eq!(a1.value(), 0x3000);
        assert_eq!(a2.value(), 0x3100);
        assert_eq!(a3.value(), 0x3200);

        assert!(smalloc.alloc(0x200, 0x100).is_err()); // only 0x100 left
    }

    #[test]
    fn free_exact_alloc() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000))
            .unwrap();

        let addr = smalloc.alloc(0x100, 0x100).unwrap();
        assert_eq!(smalloc.res.count, 1);

        assert!(smalloc.free(addr, 0x100).is_ok());
        assert_eq!(smalloc.res.count, 0);
    }

    #[test]
    fn free_first_half_of_merged_region() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000))
            .unwrap();

        let a1 = smalloc.alloc(0x100, 0x100).unwrap(); // 0x1000
        smalloc.alloc(0x100, 0x100).unwrap(); // 0x1100

        assert_eq!(smalloc.res.count, 1); // merged

        assert!(smalloc.free(a1, 0x100).is_ok());
        assert_eq!(smalloc.res.count, 1);
        assert_eq!(smalloc.res[0].start_address().value(), 0x1100);
        assert_eq!(smalloc.res[0].size(), 0x100);
    }

    #[test]
    fn free_last_half_of_merged_region() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x2000), 0x1000))
            .unwrap();

        smalloc.alloc(0x100, 0x100).unwrap(); // 0x2000
        let a1 = smalloc.alloc(0x100, 0x100).unwrap(); // 0x2100

        assert_eq!(smalloc.res.count, 1);

        assert!(smalloc.free(a1, 0x100).is_ok());
        assert_eq!(smalloc.res.count, 1);
        assert_eq!(smalloc.res[0].start_address().value(), 0x2000);
        assert_eq!(smalloc.res[0].size(), 0x100);
    }

    #[test]
    fn free_middle_of_merged_region_splits_it() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x3000), 0x1000))
            .unwrap();

        smalloc.alloc(0x100, 0x100).unwrap(); // 0x3000
        let a1 = smalloc.alloc(0x100, 0x100).unwrap(); // 0x3100
        smalloc.alloc(0x100, 0x100).unwrap(); // 0x3200

        assert_eq!(smalloc.res.count, 1); // 0x3000–0x3300

        assert!(smalloc.free(a1, 0x100).is_ok());

        assert_eq!(smalloc.res.count, 2);
        assert_eq!(smalloc.res[0].start_address().value(), 0x3000);
        assert_eq!(smalloc.res[0].size(), 0x100);
        assert_eq!(smalloc.res[1].start_address().value(), 0x3200);
        assert_eq!(smalloc.res[1].size(), 0x100);
    }

    #[test]
    fn free_entire_merged_region_removes_it() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x4000), 0x1000))
            .unwrap();

        let a1 = smalloc.alloc(0x200, 0x100).unwrap(); // 0x4000
        smalloc.alloc(0x100, 0x100).unwrap(); // 0x4200

        let merged = PhysMemoryRegion::new(a1, 0x300);
        assert!(smalloc.free(merged.start_address(), merged.size()).is_ok());
        assert_eq!(smalloc.res.count, 0);
    }

    #[test]
    fn free_nonexistent_region_fails() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x5000), 0x1000))
            .unwrap();

        let fake_addr = PA::from_value(0x6000);
        assert!(smalloc.free(fake_addr, 0x100).is_err());
    }

    #[test]
    fn alloc_after_free_reuses_space() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x6000), 0x1000))
            .unwrap();

        let a1 = smalloc.alloc(0x100, 0x100).unwrap();
        assert!(smalloc.free(a1, 0x100).is_ok());

        let a2 = smalloc.alloc(0x100, 0x100).unwrap();
        assert_eq!(a1.value(), a2.value());
    }

    #[test]
    fn double_free_fails_gracefully() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x7000), 0x1000))
            .unwrap();

        let addr = smalloc.alloc(0x100, 0x100).unwrap();
        assert!(smalloc.free(addr, 0x100).is_ok());
        assert!(matches!(
            smalloc.free(addr, 0x100),
            Err(KernelError::NoMemRegion)
        )); // already freed
    }

    #[test]
    fn alloc_fill_free_reuse() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x8000), 0x1000))
            .unwrap();

        // Allocate 4 blocks
        let blocks: Vec<_> = (0..4)
            .map(|_| smalloc.alloc(0x100, 0x100).unwrap())
            .collect();

        // Free the middle two
        assert!(smalloc.free(blocks[1], 0x100).is_ok());
        assert!(smalloc.free(blocks[2], 0x100).is_ok());

        // Allocate again — should reuse freed space
        let r1 = smalloc.alloc(0x100, 0x100).unwrap();
        let r2 = smalloc.alloc(0x100, 0x100).unwrap();

        assert_eq!(r1.value(), blocks[1].value());
        assert_eq!(r2.value(), blocks[2].value());
    }

    #[test]
    fn alloc_free_refill_stress() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0xA000), 0x1000))
            .unwrap();

        let mut addrs = vec![];

        // Fill it with aligned 0x100 blocks
        while let Ok(addr) = smalloc.alloc(0x100, 0x100) {
            addrs.push(addr);
        }

        assert!(!addrs.is_empty());
        assert!(smalloc.alloc(0x100, 0x100).is_err()); // should be full

        // Free all
        for addr in &addrs {
            assert!(smalloc.free(*addr, 0x100).is_ok());
        }

        // Should refill to same addresses
        for addr in &addrs {
            let new = smalloc.alloc(0x100, 0x100).unwrap();
            assert_eq!(new.value(), addr.value());
        }
    }

    #[test]
    fn variable_size_allocs_and_frees() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0xB000), 0x2000))
            .unwrap();

        smalloc.alloc(0x80, 0x10).unwrap();
        let a1 = smalloc.alloc(0x180, 0x20).unwrap();
        smalloc.alloc(0x100, 0x40).unwrap();

        assert!(smalloc.free(a1, 0x180).is_ok());

        let a4 = smalloc.alloc(0x100, 0x20).unwrap();
        assert_eq!(a4.value(), a1.value()); // fits exactly

        let a5 = smalloc.alloc(0x80, 0x10).unwrap(); // should use rest of freed space
        assert_eq!(a5.value(), a1.value() + 0x100);
    }

    #[test]
    fn randomized_alloc_free_simulation() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0xC000), 0x2000))
            .unwrap();

        let mut rng = rand::rngs::StdRng::seed_from_u64(0x42);
        let mut allocs: Vec<(PA, usize)> = vec![];

        for _ in 0..50 {
            if rng.random_bool(0.6) {
                let size = [0x40, 0x80, 0x100][rng.random_range(0..3)];
                let align = [0x10, 0x20, 0x40][rng.random_range(0..3)];
                if let Ok(addr) = smalloc.alloc(size, align) {
                    allocs.push((addr, size));
                }
            } else if let Some((addr, size)) = allocs.pop() {
                assert!(smalloc.free(addr, size).is_ok());
            }
        }

        // Any leftover allocations should be valid frees
        for (addr, size) in allocs {
            assert!(smalloc.free(addr, size).is_ok());
        }

        assert_eq!(smalloc.res.count, 0);
    }

    #[test]
    fn memory_region_list_move() {
        let mut smalloc = get_smalloc();
        let big_buf: &mut [MaybeUninit<PhysMemoryRegion>] = vec![MaybeUninit::uninit(); 128].leak();

        smalloc
            .add_memory(PhysMemoryRegion::new(
                PA::from_value(big_buf.as_ptr().expose_provenance()),
                big_buf.len() * core::mem::size_of::<MaybeUninit<PhysMemoryRegion>>(),
            ))
            .unwrap();

        for i in 1..=14 {
            assert!(
                smalloc
                    .add_memory(PhysMemoryRegion::new(
                        PA::from_value(big_buf.as_ptr().expose_provenance() + (i * 0x1000)),
                        0x50,
                    ))
                    .is_ok()
            );
        }

        // Ensure that the reallocation doesn't happen if it's not permitted.
        assert!(
            smalloc
                .add_memory(PhysMemoryRegion::new(
                    PA::from_value(big_buf.as_ptr().expose_provenance() + (15 * 0x1000)),
                    0x50,
                ))
                .is_err()
        );

        unsafe { smalloc.permit_region_list_reallocs() };

        assert!(
            smalloc
                .add_memory(PhysMemoryRegion::new(
                    PA::from_value(big_buf.as_ptr().expose_provenance() + (15 * 0x1000)),
                    0x50,
                ))
                .is_ok()
        );

        // We should be able to iterate through the new list.
        assert_eq!(smalloc.memory.iter().count(), 16);

        // We should have 1 reservation, the list itself.
        assert_eq!(smalloc.res.count, 1);
    }

    #[test]
    fn iter_free_no_reservations() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000))
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x3000), 0x1000))
            .unwrap();

        let free_regions: Vec<_> = smalloc.iter_free().collect();
        assert_eq!(free_regions.len(), 2);
        check_region(&free_regions[0], 0x1000, 0x1000);
        check_region(&free_regions[1], 0x3000, 0x1000);
    }

    #[test]
    fn iter_free_fully_reserved() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000))
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000))
            .unwrap();

        assert_eq!(smalloc.iter_free().count(), 0);
    }

    #[test]
    fn iter_free_prefix_reserved() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000)) // 0x1000 - 0x2000
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1000), 0x100)) // 0x1000 - 0x1100
            .unwrap();

        let free_regions: Vec<_> = smalloc.iter_free().collect();
        assert_eq!(free_regions.len(), 1);
        check_region(&free_regions[0], 0x1100, 0xF00); // 0x1100 - 0x2000
    }

    #[test]
    fn iter_free_suffix_reserved() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000)) // 0x1000 - 0x2000
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1F00), 0x100)) // 0x1F00 - 0x2000
            .unwrap();

        let free_regions: Vec<_> = smalloc.iter_free().collect();
        assert_eq!(free_regions.len(), 1);
        check_region(&free_regions[0], 0x1000, 0xF00); // 0x1000 - 0x1F00
    }

    #[test]
    fn iter_free_middle_reserved() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000)) // 0x1000 - 0x2000
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1400), 0x200)) // 0x1400 - 0x1600
            .unwrap();

        let free_regions: Vec<_> = smalloc.iter_free().collect();
        assert_eq!(free_regions.len(), 2);
        check_region(&free_regions[0], 0x1000, 0x400); // 0x1000 - 0x1400
        check_region(&free_regions[1], 0x1600, 0xA00); // 0x1600 - 0x2000
    }

    #[test]
    fn iter_free_multiple_reservations() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000)) // 0x1000 - 0x2000
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1200), 0x100)) // 0x1200 - 0x1300
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1800), 0x200)) // 0x1800 - 0x1A00
            .unwrap();

        let free_regions: Vec<_> = smalloc.iter_free().collect();
        assert_eq!(free_regions.len(), 3);
        check_region(&free_regions[0], 0x1000, 0x200); // 0x1000 - 0x1200
        check_region(&free_regions[1], 0x1300, 0x500); // 0x1300 - 0x1800
        check_region(&free_regions[2], 0x1A00, 0x600); // 0x1A00 - 0x2000
    }

    #[test]
    fn iter_free_multiple_mem_regions() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000)) // 0x1000 - 0x2000
            .unwrap();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x3000), 0x1000)) // 0x3000 - 0x4000
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x3100), 0x100)) // 0x3100 - 0x3200
            .unwrap();

        let free_regions: Vec<_> = smalloc.iter_free().collect();
        assert_eq!(free_regions.len(), 3);
        check_region(&free_regions[0], 0x1000, 0x1000);
        check_region(&free_regions[1], 0x3000, 0x100);
        check_region(&free_regions[2], 0x3200, 0xE00);
    }

    #[test]
    fn iter_free_reservation_outside_memory() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x1000))
            .unwrap();
        // This reservation is before the memory region.
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x0), 0x100))
            .unwrap();
        // This reservation is after the memory region.
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x8000), 0x100))
            .unwrap();

        let free_regions: Vec<_> = smalloc.iter_free().collect();
        assert_eq!(free_regions.len(), 1);
        check_region(&free_regions[0], 0x1000, 0x1000);
    }

    #[test]
    fn iter_free_reservation_merges_and_splits() {
        let mut smalloc = get_smalloc();
        // 0x1000 - 0x3000
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x1000), 0x2000))
            .unwrap();

        // These two reservations will be merged by `insert_region` into one: 0x1500 - 0x1A00
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1500), 0x200)) // 0x1500 - 0x1700
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1600), 0x400)) // 0x1600 - 0x1A00
            .unwrap();

        let free_regions: Vec<_> = smalloc.iter_free().collect();
        assert_eq!(smalloc.res.count, 1); // verify merge happened
        check_region(&smalloc.res[0], 0x1500, 0x500);

        assert_eq!(free_regions.len(), 2);
        check_region(&free_regions[0], 0x1000, 0x500);
        check_region(&free_regions[1], 0x1A00, 0x1600);
    }

    #[test]
    fn iter_free_no_memory_regions() {
        let smalloc = get_smalloc();
        assert_eq!(smalloc.iter_free().count(), 0);
    }

    #[test]
    fn iter_free_reservation_bigger_than_memory() {
        let mut smalloc = get_smalloc();
        smalloc
            .add_memory(PhysMemoryRegion::new(PA::from_value(0x2000), 0x100)) // 0x2000-0x2100
            .unwrap();
        smalloc
            .add_reservation(PhysMemoryRegion::new(PA::from_value(0x1000), 0x2000)) // 0x1000-0x3000
            .unwrap();

        assert_eq!(smalloc.iter_free().count(), 0);
    }
}
