//! Memory map management for a process address space.

use super::{
    address_space::UserAddressSpace,
    vmarea::{VMAPermissions, VMArea, VMAreaKind},
};
use crate::{
    error::{KernelError, Result},
    memory::{
        PAGE_MASK, PAGE_SIZE, address::VA, page::PageFrame, paging::permissions::PtePermissions,
        region::VirtMemoryRegion,
    },
};
use alloc::{collections::BTreeMap, string::String, vec::Vec};

const MMAP_BASE: usize = 0x4000_0000_0000;

/// Manages mappings in a process's address space.
pub struct MemoryMap<AS: UserAddressSpace> {
    pub(super) vmas: BTreeMap<VA, VMArea>,
    address_space: AS,
}

/// Specifies how the kernel should choose the virtual address for a mapping.
#[derive(Debug, PartialEq, Eq)]
pub enum AddressRequest {
    /// Let the kernel pick any suitable address.
    Any,
    /// Prefer the given address but fall back to any free region.
    Hint(VA),
    /// Map at exactly the given address.
    Fixed {
        /// The exact virtual address to map at.
        address: VA,
        /// If `true`, existing mappings in the range may be replaced.
        permit_overlap: bool,
    },
}

/// Describes where an `mremap` operation may place the remapped VMA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapDestination {
    /// Resize in place only.
    InPlaceOnly,
    /// Resize in place if possible, otherwise move to any free region.
    MayMove,
    /// Move the mapping to exactly this address.
    Fixed(VA),
}

impl<AS: UserAddressSpace> MemoryMap<AS> {
    /// Creates a new, empty address space.
    pub fn new() -> Result<Self> {
        Ok(Self {
            vmas: BTreeMap::new(),
            address_space: AS::new()?,
        })
    }

    pub(super) fn with_addr_spc(address_space: AS) -> Self {
        Self {
            vmas: BTreeMap::new(),
            address_space,
        }
    }

    /// Create an address space from a pre-populated list of VMAs. Used by the
    /// ELF loader.
    pub fn from_vmas(vmas: Vec<VMArea>) -> Result<Self> {
        let mut map = BTreeMap::new();

        for vma in vmas {
            map.insert(vma.region.start_address(), vma);
        }

        Ok(Self {
            vmas: map,
            address_space: AS::new()?,
        })
    }

    /// Finds the `VMArea` that contains the given virtual address.
    ///
    /// # Arguments
    /// * `addr`: The virtual address to look up.
    ///
    /// # Returns
    /// * `Some(VMArea)` if the address is part of a valid mapping.
    /// * `None` if the address is in a "hole" in the address space.
    pub fn find_vma(&self, addr: VA) -> Option<&VMArea> {
        let candidate = self.vmas.range(..=addr).next_back();

        match candidate {
            Some((_, vma)) => {
                if vma.contains_address(addr) {
                    Some(vma)
                } else {
                    None
                }
            }
            None => None, // No VMA starts at or before this address.
        }
    }

    /// Maps a region of memory.
    pub fn mmap(
        &mut self,
        requested_address: AddressRequest,
        mut len: usize,
        perms: VMAPermissions,
        kind: VMAreaKind,
        name: String,
    ) -> Result<VA> {
        if len == 0 {
            return Err(KernelError::InvalidValue);
        }

        // Ensure the length is page-aligned.
        if len & PAGE_MASK != 0 {
            len = (len & !PAGE_MASK) + PAGE_SIZE;
        }

        let region = match requested_address {
            AddressRequest::Any => self.find_free_region(len).ok_or(KernelError::NoMemory)?,
            AddressRequest::Hint(address) => {
                // Be more permissive when it's a hint.
                let address = if !address.is_page_aligned() {
                    address.page_aligned()
                } else {
                    address
                };

                let region = VirtMemoryRegion::new(address, len);

                if self.is_region_free(region) {
                    region
                } else {
                    self.find_free_region(len).ok_or(KernelError::NoMemory)?
                }
            }
            AddressRequest::Fixed {
                address,
                permit_overlap,
            } => {
                if !address.is_page_aligned() {
                    return Err(KernelError::InvalidValue);
                }

                let region = VirtMemoryRegion::new(address, len);

                if !permit_overlap && !self.is_region_free(region) {
                    return Err(KernelError::InvalidValue);
                }

                region
            }
        };

        // At this point, `start_addr` points to a valid, free region.
        // We can now create and insert the new VMA, handling merges.
        let mut new_vma = VMArea::new(region, kind, perms);

        new_vma.set_name(name);

        self.insert_and_merge(new_vma);

        Ok(region.start_address())
    }

    /// Unmaps a region of memory, similar to the `munmap` syscall.
    ///
    /// This is the most complex operation, as it may involve removing,
    /// resizing, or splitting one or more existing VMAs.
    ///
    /// # Arguments
    /// * `addr`: The starting address of the region to unmap. Must be page-aligned.
    /// * `len`: The length of the region to unmap. Will be rounded up.
    ///
    /// # Returns
    /// * `Ok(())` on success.
    /// * `Err(MunmapError)` on failure.
    pub fn munmap(&mut self, range: VirtMemoryRegion) -> Result<Vec<PageFrame>> {
        if !range.is_page_aligned() {
            return Err(KernelError::InvalidValue);
        }

        if range.size() == 0 {
            return Err(KernelError::InvalidValue);
        }

        // Ensure len is page-sized.
        self.unmap_region(range.align_to_page_boundary(), None)
    }

    /// Changes the memory protection flags for a page-aligned region.
    pub fn mprotect(
        &mut self,
        protect_region: VirtMemoryRegion,
        new_perms: VMAPermissions,
    ) -> Result<()> {
        if !protect_region.is_page_aligned() {
            return Err(KernelError::InvalidValue);
        }

        if protect_region.size() == 0 {
            return Err(KernelError::InvalidValue);
        }

        let affected_vma_addr = self
            .find_vma(protect_region.start_address())
            .map(|x| x.region.start_address())
            .ok_or(KernelError::NoMemory)?;

        let affected_vma = self
            .vmas
            .remove(&affected_vma_addr)
            .expect("Should have the same key as the start address");

        // Easy case, the entire VMA is changing.
        if affected_vma.region == protect_region {
            let old_vma = affected_vma.clone();
            let mut new_vma = old_vma.clone();
            new_vma.permissions = new_perms;

            self.insert_and_merge(new_vma.clone());
            self.address_space
                .protect_range(protect_region, new_perms.into())?;

            return Ok(());
        }

        // Next case, a sub-region of a VMA is changing, requring a split.
        if affected_vma.region.contains(protect_region) {
            let (left, right) = affected_vma.region.punch_hole(protect_region);
            let mut new_vma = affected_vma.clone().shrink_to(protect_region);
            new_vma.permissions = new_perms;

            if let Some(left) = left {
                self.insert_and_merge(affected_vma.shrink_to(left));
            }

            self.address_space
                .protect_range(protect_region, new_perms.into())?;
            self.insert_and_merge(new_vma);

            if let Some(right) = right {
                self.insert_and_merge(affected_vma.shrink_to(right));
            }

            return Ok(());
        }

        // TODO: protecting over contiguous VMAreas.
        Err(KernelError::NoMemory)
    }

    /// Remaps an existing mapping
    pub fn mremap(
        &mut self,
        old_addr: VA,
        old_len: usize,
        new_len: usize,
        destination: RemapDestination,
    ) -> Result<(VA, Vec<PageFrame>)> {
        if !old_addr.is_page_aligned() || old_len == 0 || new_len == 0 {
            return Err(KernelError::InvalidValue);
        }

        let old_len = Self::align_len(old_len);
        let new_len = Self::align_len(new_len);
        let old_region = VirtMemoryRegion::new(old_addr, old_len);

        let source_vma = self.find_vma(old_addr).cloned().ok_or(KernelError::Fault)?;

        if old_region.end_address() > source_vma.region.end_address() {
            return Err(KernelError::Fault);
        }

        if let RemapDestination::Fixed(new_addr) = destination {
            if !new_addr.is_page_aligned() {
                return Err(KernelError::InvalidValue);
            }

            let new_region = VirtMemoryRegion::new(new_addr, new_len);

            if new_region.overlaps(old_region) || new_region.overlaps(source_vma.region) {
                return Err(KernelError::InvalidValue);
            }
        }

        if old_len == new_len && !matches!(destination, RemapDestination::Fixed(_)) {
            return Ok((old_addr, Vec::new()));
        }

        if let RemapDestination::Fixed(new_addr) = destination {
            return self.move_selected_mapping(
                source_vma,
                old_region,
                VirtMemoryRegion::new(new_addr, new_len),
                true,
            );
        }

        if new_len <= old_len {
            return self.shrink_in_place(source_vma, old_region, new_len);
        }

        if self.can_expand_in_place(&source_vma, old_region, new_len) {
            return self.expand_in_place(source_vma, old_region, new_len);
        }

        let new_region = match destination {
            RemapDestination::InPlaceOnly => return Err(KernelError::NoMemory),
            RemapDestination::MayMove => self
                .find_free_region(new_len)
                .ok_or(KernelError::NoMemory)?,
            RemapDestination::Fixed(_) => unreachable!(),
        };

        self.move_selected_mapping(
            source_vma,
            old_region,
            new_region,
            matches!(destination, RemapDestination::Fixed(_)),
        )
    }

    fn align_len(len: usize) -> usize {
        if len & PAGE_MASK != 0 {
            (len & !PAGE_MASK) + PAGE_SIZE
        } else {
            len
        }
    }

    fn can_expand_in_place(
        &self,
        source_vma: &VMArea,
        old_region: VirtMemoryRegion,
        new_len: usize,
    ) -> bool {
        let new_end = old_region.start_address().add_bytes(new_len);

        if new_end <= source_vma.region.end_address() {
            return true;
        }

        self.is_region_free(VirtMemoryRegion::from_start_end_address(
            source_vma.region.end_address(),
            new_end,
        ))
    }

    fn expand_in_place(
        &mut self,
        source_vma: VMArea,
        old_region: VirtMemoryRegion,
        new_len: usize,
    ) -> Result<(VA, Vec<PageFrame>)> {
        let new_end = old_region.start_address().add_bytes(new_len);

        if new_end <= source_vma.region.end_address() {
            return Ok((old_region.start_address(), Vec::new()));
        }

        self.vmas
            .remove(&source_vma.region.start_address())
            .unwrap();

        let mut expanded_vma = source_vma;
        expanded_vma.region =
            VirtMemoryRegion::from_start_end_address(expanded_vma.region.start_address(), new_end);

        self.merge_vma(expanded_vma);

        Ok((old_region.start_address(), Vec::new()))
    }

    fn shrink_in_place(
        &mut self,
        source_vma: VMArea,
        old_region: VirtMemoryRegion,
        new_len: usize,
    ) -> Result<(VA, Vec<PageFrame>)> {
        let new_region = VirtMemoryRegion::new(old_region.start_address(), new_len);
        let removed_region = VirtMemoryRegion::from_start_end_address(
            new_region.end_address(),
            old_region.end_address(),
        );

        let freed_pages = self.address_space.unmap_range(removed_region)?;

        self.vmas
            .remove(&source_vma.region.start_address())
            .unwrap();

        if source_vma.region.start_address() < old_region.start_address() {
            self.merge_vma(
                source_vma.shrink_to(VirtMemoryRegion::from_start_end_address(
                    source_vma.region.start_address(),
                    old_region.start_address(),
                )),
            );
        }

        self.merge_vma(source_vma.shrink_to(new_region));

        if old_region.end_address() < source_vma.region.end_address() {
            self.merge_vma(
                source_vma.shrink_to(VirtMemoryRegion::from_start_end_address(
                    old_region.end_address(),
                    source_vma.region.end_address(),
                )),
            );
        }

        Ok((old_region.start_address(), freed_pages))
    }

    fn relocate_vma(vma: VMArea, new_region: VirtMemoryRegion) -> VMArea {
        let mut moved_vma = vma;
        moved_vma.region = new_region;

        if let VMAreaKind::File(mapping) = &mut moved_vma.kind {
            mapping.len = core::cmp::min(mapping.len, new_region.size() as u64);
        }

        moved_vma
    }

    fn move_selected_mapping(
        &mut self,
        source_vma: VMArea,
        old_region: VirtMemoryRegion,
        new_region: VirtMemoryRegion,
        clobber_target: bool,
    ) -> Result<(VA, Vec<PageFrame>)> {
        let mut freed_pages = Vec::new();

        if clobber_target {
            freed_pages.append(&mut self.unmap_region(new_region, None)?);
        }

        let preserved_len = core::cmp::min(old_region.size(), new_region.size());
        let mut newly_mapped = Vec::new();

        if preserved_len != 0 {
            let preserved_old = VirtMemoryRegion::new(old_region.start_address(), preserved_len);
            let preserved_new = VirtMemoryRegion::new(new_region.start_address(), preserved_len);

            for (old_page, new_page) in preserved_old.iter_pages().zip(preserved_new.iter_pages()) {
                if let Some(page_info) = self.address_space.translate(old_page) {
                    if let Err(err) =
                        self.address_space
                            .map_page(page_info.pfn, new_page, page_info.perms)
                    {
                        for mapped_page in newly_mapped {
                            let _ = self.address_space.unmap(mapped_page);
                        }

                        return Err(err);
                    }

                    newly_mapped.push(new_page);
                }
            }

            let _ = self.address_space.unmap_range(preserved_old)?;
        }

        if old_region.size() > preserved_len {
            freed_pages.append(&mut self.address_space.unmap_range(
                VirtMemoryRegion::from_start_end_address(
                    old_region.start_address().add_bytes(preserved_len),
                    old_region.end_address(),
                ),
            )?);
        }

        self.vmas
            .remove(&source_vma.region.start_address())
            .unwrap();

        if source_vma.region.start_address() < old_region.start_address() {
            self.merge_vma(
                source_vma.shrink_to(VirtMemoryRegion::from_start_end_address(
                    source_vma.region.start_address(),
                    old_region.start_address(),
                )),
            );
        }

        if old_region.end_address() < source_vma.region.end_address() {
            self.merge_vma(
                source_vma.shrink_to(VirtMemoryRegion::from_start_end_address(
                    old_region.end_address(),
                    source_vma.region.end_address(),
                )),
            );
        }

        let selected_vma = source_vma.shrink_to(old_region);
        self.merge_vma(Self::relocate_vma(selected_vma, new_region));

        Ok((new_region.start_address(), freed_pages))
    }

    /// Checks if a given virtual memory region is completely free.
    fn is_region_free(&self, region: VirtMemoryRegion) -> bool {
        // Find the VMA that might overlap with the start of our desired region.
        let candidate = self.vmas.range(..=region.start_address()).next_back();

        if let Some((_, prev_vma)) = candidate {
            // If the previous VMA extends into our desired region, it's not
            // free.
            if prev_vma.region.end_address() > region.start_address() {
                return false;
            }
        }

        // Check if the next VMA starts within our desired region.
        if let Some((next_vma_start, _)) = self.vmas.range(region.start_address()..).next()
            && *next_vma_start < region.end_address()
        {
            false
        } else {
            true
        }
    }

    /// Finds a free region of at least `len` bytes. Searches downwards from
    /// `MMAP_BASE`.
    fn find_free_region(&self, len: usize) -> Option<VirtMemoryRegion> {
        let mut last_vma_end = VA::from_value(MMAP_BASE);

        // Iterate through VMAs in reverse order to find a gap.
        for (_, vma) in self.vmas.iter().rev() {
            let vma_start = vma.region.start_address();
            let vma_end = vma.region.end_address();

            if last_vma_end >= vma_end {
                let gap_start = vma_end;
                let gap_size = last_vma_end.value() - gap_start.value();

                if gap_size >= len {
                    // Found a large enough gap. Place the new mapping at the top of it.
                    return Some(VirtMemoryRegion::new(
                        VA::from_value(last_vma_end.value() - len),
                        len,
                    ));
                }
            }
            last_vma_end = vma_start;
        }

        // Check the final gap at the beginning of the mmap area.
        if last_vma_end.value() >= len {
            Some(VirtMemoryRegion::new(
                VA::from_value(last_vma_end.value() - len),
                len,
            ))
        } else {
            None
        }
    }

    /// Inserts a new VMA, handling overlaps and merging it with neighbors if
    /// possible.
    pub(super) fn insert_and_merge(&mut self, vma: VMArea) {
        let _ = self.unmap_region(vma.region, Some(vma.clone()));
        self.merge_vma(vma);
    }

    fn merge_vma(&mut self, mut vma: VMArea) {
        // Try to merge with next VMA.
        if let Some(next_vma) = self.vmas.get(&vma.region.end_address())
            && vma.can_merge_with(next_vma)
        {
            // The properties are compatible. We take the region from the
            // next VMA, remove it from the map, and expand our new VMA
            // to cover the combined area.
            let next_vma_region = self
                .vmas
                .remove(&next_vma.region.start_address())
                .unwrap() // Should not fail, as we just got this VMA.
                .region;
            vma.region.expand_by(next_vma_region.size());
        }

        // Try to merge with the previous VMA.
        if let Some((_key, prev_vma)) = self
            .vmas
            .range_mut(..vma.region.start_address())
            .next_back()
            // Check if it's contiguous and compatible.
            && prev_vma.region.end_address() == vma.region.start_address()
            && prev_vma.can_merge_with(&vma)
        {
            // The VMAs are mergeable. Expand the previous VMA to absorb the
            // new one's region.
            prev_vma.region.expand_by(vma.region.size());
            return;
        }

        // If we didn't merge into a previous VMA, insert the new (and possibly
        // already merged with the next) VMA into the map.
        self.vmas.insert(vma.region.start_address(), vma);
    }

    /// Fixup the unerlying page tables whenever a VMArea is being modified.
    fn fixup_pg_tables(
        &mut self,
        fixup_region: VirtMemoryRegion,
        old_vma: VMArea,
        new_vma: Option<VMArea>,
    ) -> Result<Vec<PageFrame>> {
        let intersecting_region = fixup_region.intersection(old_vma.region);

        if let Some(intersection) = intersecting_region {
            match new_vma {
                Some(new_vma) => {
                    // We always unmap if file backing-stores are involoved.
                    if old_vma.is_file_backed() || new_vma.is_file_backed() {
                        self.address_space.unmap_range(intersection)
                    } else {
                        // the VMAs are anonymously mapped. Preserve data.
                        if new_vma.permissions != old_vma.permissions {
                            self.address_space
                                .protect_range(
                                    intersection,
                                    PtePermissions::from(new_vma.permissions),
                                )
                                .map(|_| Vec::new())
                        } else {
                            // If permissions match, fixup is a noop
                            Ok(Vec::new())
                        }
                    }
                }
                None => self.address_space.unmap_range(intersection),
            }
        } else {
            Ok(Vec::new())
        }
    }

    /// Create a hole in the address space identifed by the region. If regions
    /// overlap, shrink them. If regions lie inside the region, remove them.
    ///
    /// This function is called by both the unmap code (replace_with = None),
    /// and the insert_and_merge code (replace_with = Some(<new vma>)). The
    /// `replace_with` parameter can be used to update the underlying page
    /// tables accordingly.
    ///
    /// # Returns
    /// A list of all pages that were unmapped.
    fn unmap_region(
        &mut self,
        unmap_region: VirtMemoryRegion,
        replace_with: Option<VMArea>,
    ) -> Result<Vec<PageFrame>> {
        let mut affected_vmas = Vec::new();
        let unmap_start = unmap_region.start_address();
        let unmap_end = unmap_region.end_address();
        let mut pages_unmapped = Vec::new();

        // Find all VMAs that intersect with the unmap region. Start with the
        // VMA that could contain the start address.
        if let Some((_, vma)) = self.vmas.range(..unmap_start).next_back()
            && vma.region.end_address() > unmap_start
        {
            affected_vmas.push(vma.clone());
        }

        // Add all other VMAs that start within the unmap region.
        for (_, vma) in self.vmas.range(unmap_start..) {
            if vma.region.start_address() < unmap_end {
                affected_vmas.push(vma.clone());
            } else {
                break; // We're past the unmap region now.
            }
        }

        if affected_vmas.is_empty() {
            return Ok(Vec::new());
        }

        for vma in affected_vmas {
            let vma_start = vma.region.start_address();
            let vma_end = vma.region.end_address();

            self.vmas.remove(&vma_start).unwrap();

            pages_unmapped.append(&mut self.fixup_pg_tables(
                unmap_region,
                vma.clone(),
                replace_with.clone(),
            )?);

            // VMA is completely contained within the unmap region. Handled by
            // just removing it.

            // VMA needs to be split (unmap punches a hole).
            if vma_start < unmap_start && vma_end > unmap_end {
                // Create left part.
                let left_region =
                    VirtMemoryRegion::new(vma_start, unmap_start.value() - vma_start.value());
                let left_vma = vma.clone_with_new_region(left_region);
                self.vmas.insert(left_vma.region.start_address(), left_vma);

                // Create right part.
                let right_region =
                    VirtMemoryRegion::new(unmap_end, vma_end.value() - unmap_end.value());
                let right_vma = vma.clone_with_new_region(right_region);
                self.vmas
                    .insert(right_vma.region.start_address(), right_vma);

                continue;
            }

            // VMA needs to be truncated at the end.
            if vma_start < unmap_start {
                let new_size = unmap_start.value() - vma_start.value();
                let new_region = VirtMemoryRegion::new(vma_start, new_size);
                let new_vma = vma.clone_with_new_region(new_region);
                self.vmas.insert(new_vma.region.start_address(), new_vma);
            }

            // VMA needs to be truncated at the beginning.
            if vma_end > unmap_end {
                let new_start = unmap_end;
                let new_size = vma_end.value() - new_start.value();
                let new_region = VirtMemoryRegion::new(new_start, new_size);
                let mut new_vma = vma.clone_with_new_region(new_region);

                // Adjust file mapping offset if it's a file-backed VMA.
                if let VMAreaKind::File(mapping) = &mut new_vma.kind {
                    let offset_change = new_start.value() - vma_start.value();
                    mapping.offset += offset_change as u64;
                }

                self.vmas.insert(new_vma.region.start_address(), new_vma);
            }
        }

        Ok(pages_unmapped)
    }

    /// Attempts to clone this memory map, sharing any already-mapped writable
    /// pages as CoW pages. If the VMA isn't writable, the ref count is
    /// incremented.
    pub fn clone_as_cow(&mut self) -> Result<Self> {
        let mut new_as = AS::new()?;
        let new_vmas = self.vmas.clone();

        for vma in new_vmas.values() {
            let mut pte_perms = PtePermissions::from(vma.permissions);

            // Mark all writable pages as CoW.
            if pte_perms.is_write() {
                pte_perms = pte_perms.into_cow();
            }

            self.address_space.protect_and_clone_region(
                vma.region.align_to_page_boundary(),
                &mut new_as,
                pte_perms,
            )?;
        }

        Ok(Self {
            vmas: new_vmas,
            address_space: new_as,
        })
    }

    /// Returns a mutable reference to the underlying address space.
    pub fn address_space_mut(&mut self) -> &mut AS {
        &mut self.address_space
    }

    /// Returns the number of VMAs in this memory map.
    pub fn vma_count(&self) -> usize {
        self.vmas.len()
    }

    /// Returns an iterator over all VMAs in address order.
    pub fn iter_vmas(&self) -> impl Iterator<Item = &VMArea> {
        self.vmas.values()
    }
}

#[cfg(test)]
#[allow(missing_docs)]
pub mod tests;
