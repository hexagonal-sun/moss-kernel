//! Virtual Memory Areas (VMAs) within a process's address space.
//!
//! The [`VMArea`] represents a contiguous range of virtual memory with a
//! uniform set of properties, such as permissions and backing source. A
//! process's memory map is composed of a set of these VMAs.
//!
//! A VMA can be either:
//! - File-backed (via [`VMAreaKind::File`]): Used for loading executable code
//!   and initialized data from files, most notably ELF binaries.
//! - Anonymous (via [`VMAreaKind::Anon`]): Used for demand-zeroed memory like
//!   the process stack, heap, and BSS sections.
use core::cmp;

use crate::{
    fs::Inode,
    memory::{PAGE_MASK, PAGE_SIZE, address::VA, region::VirtMemoryRegion},
};
use alloc::sync::Arc;
use object::{
    Endian,
    elf::{PF_R, PF_W, PF_X, ProgramHeader64},
    read::elf::ProgramHeader,
};

/// Describes the permissions assigned for this VMA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VMAPermissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl VMAPermissions {
    pub const fn rw() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
        }
    }

    pub const fn rx() -> Self {
        Self {
            read: true,
            write: false,
            execute: true,
        }
    }

    pub const fn ro() -> Self {
        Self {
            read: true,
            write: false,
            execute: false,
        }
    }
}

/// Describes the kind of access that occured during a page fault.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    /// The CPU attempted to read the faulting address.
    Read,
    /// The CPU attempted to write to the faulting address.
    Write,
    /// The CPU attempted to execute the instruciton at the faulting address.
    Execute,
}

/// The result of checking a memory access against a `VMArea`.
///
/// This enum tells the fault handler how to proceed.
#[derive(Debug, PartialEq, Eq)]
pub enum FaultValidation {
    /// The access is valid. The address is within the VMA and has the required
    /// permissions. The fault handler should proceed with populating the page.
    Valid,

    /// The address is not within this VMA's region. The fault handler should
    /// continue searching other VMAs.
    NotPresent,

    /// The address is within this VMA's region, but the access kind is not
    /// permitted (e.g., writing to a read-only page). This is a definitive
    /// segmentation fault. The fault handler can immediately stop its search
    /// and terminate the process.
    PermissionDenied,
}

/// Describes a read operation from a file required to satisfy a page fault.
pub struct VMAFileRead {
    /// The absolute offset into the backing file to start reading from.
    pub file_offset: u64,
    /// The offset into the destination page where the read data should be
    /// written.
    pub page_offset: usize,
    /// The number of bytes to read from the file and write to the page.
    pub read_len: usize,
    /// The file that backs this VMA mapping.
    pub inode: Arc<dyn Inode>,
}

/// Represents a mapping to a region of a file that backs a `VMArea`.
///
/// This specifies a "slice" of a file, defined by an `offset` and `len`, that
/// contains the initialized data for a memory segment (e.g., the .text or .data
/// sections of an ELF binary).
#[derive(Clone)]
pub struct VMFileMapping {
    pub(super) file: Arc<dyn Inode>,
    pub(super) offset: u64,
    pub(super) len: u64,
}

impl PartialEq for VMFileMapping {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.file, &other.file) && self.offset == other.offset && self.len == other.len
    }
}

impl VMFileMapping {
    /// Returns a clone of the reference-counted `Inode` for this mapping.
    pub fn file(&self) -> Arc<dyn Inode> {
        self.file.clone()
    }

    /// Returns the starting offset of the mapping's data within the file.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the length of the mapping's data within the file (`p_filesz`).
    pub fn file_len(&self) -> u64 {
        self.len
    }
}

/// Defines the backing source for a `VMArea`.
#[derive(Clone, PartialEq)]
pub enum VMAreaKind {
    /// The VMA is backed by a file.
    ///
    /// On a page fault, the kernel will read data from the specified file
    /// region to populate the page. Any part of the VMA's memory region that
    /// extends beyond the file mapping's length (`p_memsz` > `p_filesz`) is
    /// treated as BSS and will be zero-filled.
    File(VMFileMapping),

    /// The VMA is an anonymous, demand-zeroed mapping.
    ///
    /// It has no backing file. On a page fault, the kernel will provide a new
    /// physical page that has been zero-filled. This is used for the heap,
    /// and the stack.
    Anon,
}

impl VMAreaKind {
    pub fn new_anon() -> Self {
        Self::Anon
    }

    pub fn new_file(file: Arc<dyn Inode>, offset: u64, len: u64) -> Self {
        Self::File(VMFileMapping { file, offset, len })
    }
}

/// A Virtual Memory Area (VMA).
///
/// This represents a contiguous region of virtual memory within a process's
/// address space that shares a common set of properties, such as memory
/// permissions and backing source. It is the kernel's primary abstraction for
/// managing a process's memory layout.
#[derive(Clone, PartialEq)]
pub struct VMArea {
    pub(super) region: VirtMemoryRegion,
    pub(super) kind: VMAreaKind,
    pub(super) permissions: VMAPermissions,
}

impl VMArea {
    /// Creates a new `VMArea`.
    ///
    /// # Arguments
    /// * `region`: The virtual address range for this VMA.
    /// * `kind`: The backing source (`File` or `Anon`).
    /// * `permissions`: The memory permissions for the region.
    pub fn new(region: VirtMemoryRegion, kind: VMAreaKind, permissions: VMAPermissions) -> Self {
        Self {
            region,
            kind,
            permissions,
        }
    }

    /// Creates a file-backed `VMArea` directly from an ELF program header.
    ///
    /// This is a convenience function used by the ELF loader. It parses the
    /// header to determine the virtual address range, file mapping details, and
    /// memory permissions.
    ///
    /// Note: If a program header's VA isn't page-aligned this function will
    /// align it down and addjust the offset and size accordingly.
    ///
    /// # Arguments
    /// * `f`: A handle to the ELF file's inode.
    /// * `hdr`: The ELF program header (`LOAD` segment) to create the VMA from.
    /// * `endian`: The endianness of the ELF file, for correctly parsing header fields.
    pub fn from_pheader<E: Endian>(
        f: Arc<dyn Inode>,
        hdr: ProgramHeader64<E>,
        endian: E,
    ) -> VMArea {
        let mut permissions = VMAPermissions {
            read: false,
            write: false,
            execute: false,
        };

        if hdr.p_flags(endian) & PF_X != 0 {
            permissions.execute = true;
        }

        if hdr.p_flags(endian) & PF_R != 0 {
            permissions.read = true;
        }

        if hdr.p_flags(endian) & PF_W != 0 {
            permissions.write = true;
        }

        let mappable_region = VirtMemoryRegion::new(
            VA::from_value(hdr.p_vaddr(endian) as usize),
            hdr.p_memsz(endian) as usize,
        )
        .to_mappable_region();

        Self {
            region: mappable_region.region(),
            kind: VMAreaKind::File(VMFileMapping {
                file: f,
                offset: hdr.p_offset(endian) - mappable_region.offset() as u64,
                len: hdr.p_filesz(endian) + mappable_region.offset() as u64,
            }),
            permissions,
        }
    }

    /// Checks if a page fault is valid for this VMA.
    ///
    /// This is the primary function for a fault handler to use. It verifies
    /// both that the faulting address is within the VMA's bounds and that the
    /// type of access (read, write, or execute) is permitted by the VMA's
    /// permissions.
    ///
    /// # Returns
    ///
    /// - [`AccessValidation::Valid`]: If the address and permissions are valid.
    /// - [`AccessValidation::NotPresent`]: If the address is outside this VMA.
    /// - [`AccessValidation::PermissionDenied`]: If the address is inside this
    ///   VMA but the access is not allowed. This allows the caller to immediately
    ///   identify a segmentation fault without checking other VMAs.
    pub fn validate_fault(&self, addr: VA, kind: AccessKind) -> FaultValidation {
        if !self.contains_address(addr) {
            return FaultValidation::NotPresent;
        }

        // The address is in our region. Now, check permissions.
        let permissions_ok = match kind {
            AccessKind::Read => self.permissions.read,
            AccessKind::Write => self.permissions.write,
            AccessKind::Execute => self.permissions.execute,
        };

        if permissions_ok {
            FaultValidation::Valid
        } else {
            FaultValidation::PermissionDenied
        }
    }

    /// Returns a reference to the kind of backing for this VMA.
    pub fn kind(&self) -> &VMAreaKind {
        &self.kind
    }

    /// Resolves a page fault within this VMA.
    ///
    /// If the fault is in a region backed by a file, this function calculates
    /// the file offset and page memory offset required to load the data. It
    /// correctly handles the ELF alignment congruence rule and mixed pages
    /// containing both file-backed data and BSS data.
    ///
    /// # Arguments
    /// * `faulting_addr`: The virtual address that caused the fault.
    ///
    /// # Returns
    /// * `Some(VMAFileRead)` if the faulting page contains any data that must
    ///   be loaded from the file. The caller is responsible for zeroing the
    ///   page *before* performing the read.
    /// * `None` if the VMA is anonymous (`Anon`) or if the faulting page is
    ///   purely BSS (i.e., contains no data from the file) and should simply be
    ///   zero-filled.
    pub fn resolve_fault(&self, faulting_addr: VA) -> Option<VMAFileRead> {
        // Match on the kind of VMA. If it's anonymous, there's no file to read from.
        let mapping = match &self.kind {
            VMAreaKind::Anon => return None,
            VMAreaKind::File(mapping) => mapping,
        };

        let vma_start_addr = self.region.start_address();
        let p_vaddr = vma_start_addr.value() as u64;
        let p_offset = mapping.offset();
        let p_filesz = mapping.file_len();
        let vaddr_page_offset = p_vaddr & PAGE_MASK as u64;

        // The virtual address where the page-aligned mapping starts.
        let map_start_vaddr = vma_start_addr.page_aligned().value() as u64;

        // The file offset corresponding to the start of the page-aligned
        // mapping.
        let map_start_foffset = p_offset - vaddr_page_offset;

        let fault_page_vaddr = faulting_addr.page_aligned().value() as u64;

        let fault_page_offset_in_map = fault_page_vaddr.saturating_sub(map_start_vaddr);
        let file_offset_for_page_start = map_start_foffset + fault_page_offset_in_map;

        let page_write_offset = if fault_page_vaddr == map_start_vaddr {
            vaddr_page_offset as usize
        } else {
            0
        };

        // The starting point in the file we need is the max of our calculated
        // start and the actual start of the segment's data in the file.
        let read_start_file_offset = core::cmp::max(file_offset_for_page_start, p_offset);

        // The end point in the file is the segment's data end.
        let read_end_file_offset = p_offset + p_filesz;

        // The number of bytes to read is the length of this intersection.
        let read_len = read_end_file_offset.saturating_sub(read_start_file_offset) as usize;

        // The final read length cannot exceed what's left in the page.
        let final_read_len = core::cmp::min(read_len, PAGE_SIZE - page_write_offset);

        if final_read_len == 0 {
            // There is no file data to read for this page. It is either fully
            // BSS or a hole in the file mapping. The caller should use a zeroed
            // page.
            return None;
        }

        Some(VMAFileRead {
            file_offset: read_start_file_offset,
            page_offset: page_write_offset,
            read_len: final_read_len,
            inode: mapping.file.clone(),
        })
    }

    pub fn permissions(&self) -> VMAPermissions {
        self.permissions
    }

    pub fn contains_address(&self, addr: VA) -> bool {
        self.region.contains_address(addr)
    }

    /// Checks if this VMA can be merged with an adjacent one.
    ///
    /// This function assumes the other VMA is immediately adjacent in memory.
    /// Merging is possible if permissions are identical and the backing storage
    /// is of a compatible and contiguous nature.
    pub(super) fn can_merge_with(&self, other: &VMArea) -> bool {
        if self.permissions != other.permissions {
            return false;
        }

        match (&self.kind, &other.kind) {
            (VMAreaKind::Anon, VMAreaKind::Anon) => true,

            (VMAreaKind::File(self_map), VMAreaKind::File(other_map)) => {
                // Check that they point to the same inode.
                let same_file = Arc::ptr_eq(&self_map.file, &other_map.file);

                // Check that the file offsets are contiguous. `other` VMA's
                // offset must be `self`'s offset + `self`'s size.
                let contiguous_offset =
                    other_map.offset == self_map.offset + self.region.size() as u64;

                same_file && contiguous_offset
            }

            _ => false,
        }
    }

    #[must_use]
    pub(super) fn clone_with_new_region(&self, new_region: VirtMemoryRegion) -> Self {
        let mut clone = self.clone();

        clone.region = new_region;

        clone
    }

    /// Returns true if the VMA is backed by a file. False if it's an anonymous
    /// mapping.
    pub fn is_file_backed(&self) -> bool {
        matches!(self.kind, VMAreaKind::File(_))
    }

    /// Shrink this VMA's region to `new_region`, recalculating file offsets,
    /// for file mappings.
    #[must_use]
    pub(crate) fn shrink_to(&self, new_region: VirtMemoryRegion) -> Self {
        debug_assert!(self.region.contains(new_region));

        let mut new_vma = self.clone_with_new_region(new_region);

        match self.kind {
            VMAreaKind::File(ref vmfile_mapping) => {
                let start_offset =
                    new_region.start_address().value() - self.region.start_address().value();

                let new_sz = cmp::min(
                    vmfile_mapping.len.saturating_sub(start_offset as u64),
                    new_region.size() as u64,
                );

                if new_sz == 0 {
                    // convert this VMA into an anonymous VMA, since we've
                    // shrunk past the file mapping.
                    new_vma.kind = VMAreaKind::Anon;
                } else {
                    new_vma.kind = VMAreaKind::File(VMFileMapping {
                        file: vmfile_mapping.file.clone(),
                        offset: vmfile_mapping.offset + start_offset as u64,
                        len: new_sz,
                    });
                }

                new_vma
            }
            VMAreaKind::Anon => new_vma,
        }
    }
}

#[cfg(test)]
pub mod tests {
    use crate::fs::InodeId;

    use super::*;
    use async_trait::async_trait;

    #[derive(Debug)]
    pub struct DummyTestInode;

    #[async_trait]
    impl Inode for DummyTestInode {
        fn id(&self) -> InodeId {
            unreachable!("Not called")
        }
    }

    pub fn create_test_vma(vaddr: usize, memsz: usize, file_offset: u64, filesz: u64) -> VMArea {
        let dummy_inode = Arc::new(DummyTestInode);
        VMArea::new(
            VirtMemoryRegion::new(VA::from_value(vaddr), memsz),
            VMAreaKind::File(VMFileMapping {
                file: dummy_inode,
                offset: file_offset,
                len: filesz,
            }),
            VMAPermissions::rw(),
        )
    }

    #[test]
    fn simple_aligned_segment() {
        // A segment that is perfectly aligned to page boundaries.
        let vma = create_test_vma(0x20000, 0x2000, 0x4000, 0x2000);

        // Fault in the first page
        let fault_addr = VA::from_value(0x20500);
        let result = vma.resolve_fault(fault_addr).expect("Should resolve");

        assert_eq!(result.file_offset, 0x4000);
        assert_eq!(result.page_offset, 0);
        assert_eq!(result.read_len, PAGE_SIZE);

        // Fault in the second page
        let fault_addr = VA::from_value(0x21500);
        let result = vma.resolve_fault(fault_addr).expect("Should resolve");

        assert_eq!(result.file_offset, 0x5000);
        assert_eq!(result.page_offset, 0);
        assert_eq!(result.read_len, PAGE_SIZE);
    }

    #[test]
    fn unaligned_segment() {
        // vaddr and file offset are not page-aligned, but are congruent modulo
        // the page size. vaddr starts 0xf00 bytes into page 0x40000. filesz
        // (0x300) is smaller than a page.
        let vma = create_test_vma(0x40100, 0x300, 0x5100, 0x300);

        // Fault anywhere in this small segment
        let fault_addr = VA::from_value(0x40280);
        let result = vma.resolve_fault(fault_addr).expect("Should resolve");

        // The handler needs to map page 0x40000.
        // The read must start from the true file offset (p_offset).
        assert_eq!(result.file_offset, 0x5100);
        // The data must be written 0xf00 bytes into the destination page.
        assert_eq!(result.page_offset, 0x100);
        // We only read the number of bytes specified in filesz.
        assert_eq!(result.read_len, 0x300);
    }

    #[test]
    fn unaligned_segment_spanning_pages() {
        // An unaligned segment that is large enough to span multiple pages.
        // Starts at 0x40F00, size 0x800. Ends at 0x41700. Covers the last 0x100
        // bytes of page 0x40000 and the first 0x700 of page 0x41000.
        let vma = create_test_vma(0x40F00, 0x800, 0x5F00, 0x800);

        let fault_addr1 = VA::from_value(0x40F80);
        let result1 = vma
            .resolve_fault(fault_addr1)
            .expect("Should resolve first page");
        assert_eq!(result1.file_offset, 0x5F00, "File offset for first page");
        assert_eq!(result1.page_offset, 0xF00, "Page offset for first page");
        assert_eq!(result1.read_len, 0x100, "Read length for first page");

        let fault_addr2 = VA::from_value(0x41200);
        let result2 = vma
            .resolve_fault(fault_addr2)
            .expect("Should resolve second page");
        // The read should start where the first one left off: 0x5F00 + 0x100
        assert_eq!(result2.file_offset, 0x6000, "File offset for second page");
        assert_eq!(result2.page_offset, 0, "Page offset for second page");
        // The remaining bytes of the file need to be read. 0x800 - 0x100
        assert_eq!(result2.read_len, 0x700, "Read length for second page");
    }

    #[test]
    fn mixed_data_bss_page() {
        // A segment where the data ends partway through a page, and BSS begins.
        // filesz = 0x1250 (one full page, and 0x250 bytes into the second page)
        // memsz = 0x3000 (lots of BSS)
        let vma = create_test_vma(0x30000, 0x3000, 0x8000, 0x1250);

        let fault_addr_data = VA::from_value(0x30100);
        let result_data = vma
            .resolve_fault(fault_addr_data)
            .expect("Should resolve full data page");
        assert_eq!(result_data.file_offset, 0x8000);
        assert_eq!(result_data.page_offset, 0);
        assert_eq!(result_data.read_len, 0x1000);

        let fault_addr_mixed_data = VA::from_value(0x31100);
        let result_mixed = vma
            .resolve_fault(fault_addr_mixed_data)
            .expect("Should resolve mixed page");
        assert_eq!(result_mixed.file_offset, 0x9000);
        assert_eq!(result_mixed.page_offset, 0);
        assert_eq!(
            result_mixed.read_len, 0x250,
            "Should only read the remaining file bytes"
        );

        // Fault in the *BSS* part of the same mixed page. This should trigger
        // the exact same read operation.
        let fault_addr_mixed_bss = VA::from_value(0x31800);
        let result_mixed_2 = vma
            .resolve_fault(fault_addr_mixed_bss)
            .expect("Should resolve mixed page from BSS fault");
        assert_eq!(result_mixed_2.file_offset, 0x9000);
        assert_eq!(result_mixed_2.page_offset, 0);
        assert_eq!(result_mixed_2.read_len, 0x250);
    }

    #[test]
    fn pure_bss_fault() {
        // Using the same VMA as the mixed test, but faulting in a page that is
        // entirely BSS.
        let vma = create_test_vma(0x30000, 0x3000, 0x8000, 0x1250);
        let fault_addr = VA::from_value(0x32100); // 0x30000 + 0x1250 is the start of BSS.

        let result = vma.resolve_fault(fault_addr);
        assert!(result.is_none(), "Pure BSS fault should return None");
    }

    #[test]
    fn anonymous_mapping() {
        // An anonymous VMA should never result in a file read.
        let vma = VMArea::new(
            VirtMemoryRegion::new(VA::from_value(0x50000), 0x1000),
            VMAreaKind::Anon,
            VMAPermissions::rw(),
        );
        let fault_addr = VA::from_value(0x50500);

        let result = vma.resolve_fault(fault_addr);
        assert!(result.is_none(), "Anonymous VMA fault should return None");
    }

    #[test]
    fn shrink_anonymous_vma() {
        // Easy case: Standard shrinking of an anonymous region
        let vma = VMArea::new(
            VirtMemoryRegion::new(VA::from_value(0x5000), 0x4000),
            VMAreaKind::Anon,
            VMAPermissions::rw(),
        );

        // Shrink to the middle.
        let new_region = VirtMemoryRegion::new(VA::from_value(0x6000), 0x1000);
        let result = vma.shrink_to(new_region);

        assert_eq!(result.region, new_region);
        assert!(matches!(result.kind, VMAreaKind::Anon));
    }

    #[test]
    fn shrink_file_vma_from_front() {
        // Scenario: [  File (0x4000)  ]
        // Cut:      xx[ File (0x1000) ]
        // Expect: Offset increases, Length decreases

        let vma = create_test_vma(0x1000, 0x4000, 0x0, 0x4000);
        let new_region = VirtMemoryRegion::new(VA::from_value(0x4000), 0x1000);

        let result = vma.shrink_to(new_region);

        assert_eq!(result.region, new_region);
        match result.kind {
            VMAreaKind::File(vmfile_mapping) => {
                assert_eq!(vmfile_mapping.len, 0x1000);
                assert_eq!(vmfile_mapping.offset, 0x3000);
            }
            _ => panic!("Expected File VMA"),
        }
    }

    #[test]
    fn shrink_file_vma_from_end() {
        // Scenario: [  File (0x4000)  ]
        // Cut:      [ File (0x2000) ]xx

        let vma = create_test_vma(0x1000, 0x4000, 0x0, 0x4000);
        let new_region = VirtMemoryRegion::new(VA::from_value(0x1000), 0x2000);

        let result = vma.shrink_to(new_region);

        assert_eq!(result.region, new_region);

        match result.kind {
            VMAreaKind::File(vmfile_mapping) => {
                // Offset shouldn't change
                assert_eq!(vmfile_mapping.offset, 0x0);
                assert_eq!(vmfile_mapping.len, 0x2000);
            }
            _ => panic!("Expected File VMA"),
        }
    }

    #[test]
    fn shrink_file_vma_both_sides() {
        // Scenario: [   File (0x4000)   ]
        // Cut:      xx[ File (0x1000) ]xx
        let vma = create_test_vma(0x1000, 0x4000, 0x0, 0x4000);
        let new_region = VirtMemoryRegion::new(VA::from_value(0x2000), 0x1000);

        let result = vma.shrink_to(new_region);

        match result.kind {
            VMAreaKind::File(vmfile_mapping) => {
                assert_eq!(vmfile_mapping.offset, 0x1000);
                assert_eq!(vmfile_mapping.len, 0x1000);
            }
            _ => panic!("Expected File VMA"),
        }
    }

    #[test]
    fn shrink_mixed_vma_past_file_boundary_becomes_anon() {
        // Scenario: [ File (0x2000) | Anon (0x2000) ]
        // Cut front by 0x3000.    xx[ Anon (0x1000) ]

        let vma = create_test_vma(0x1000, 0x4000, 0x0, 0x2000);

        let new_region = VirtMemoryRegion::new(VA::from_value(0x4000), 0x1000);

        let result = vma.shrink_to(new_region);

        assert_eq!(result.region, new_region);
        assert!(
            matches!(result.kind, VMAreaKind::Anon),
            "Should have converted to Anon because we skipped the file part"
        );
    }

    #[test]
    fn shrink_mixed_vma_keep_file_part() {
        // Scenario: [ File (0x2000) | Anon (0x2000) ]
        // cut:      xx[ File(0x1000)| Anon (0x2000) ]
        let vma = create_test_vma(0x1000, 0x4000, 0x0, 0x2000);
        let new_region = VirtMemoryRegion::new(VA::from_value(0x2000), 0x3000);

        let result = vma.shrink_to(new_region);

        match result.kind {
            VMAreaKind::File(mapping) => {
                assert_eq!(mapping.offset, 0x1000);
                assert_eq!(mapping.len, 0x1000);
            }
            _ => panic!("Expected File VMA"),
        }
    }

    #[test]
    fn shrink_mixed_vma_from_end_remove_anon_part() {
        // Scenario: [ File (0x2000) | Anon (0x2000) ]
        // Cut:      [ File (0x2000) ]xx
        let vma = create_test_vma(0x1000, 0x4000, 0x0, 0x2000);
        let new_region = VirtMemoryRegion::new(VA::from_value(0x1000), 0x2000);

        let result = vma.shrink_to(new_region);

        match result.kind {
            VMAreaKind::File(mapping) => {
                assert_eq!(mapping.offset, 0x0);
                assert_eq!(mapping.len, 0x2000);
            }
            _ => panic!("Expected File VMA"),
        }
    }

    #[test]
    fn shrink_mixed_vma_exact_boundary() {
        // Scenario: [ File (0x2000) | Anon (0x2000) ]
        // Cut:                   xxx[ Anon (0x2000) ]

        let vma = create_test_vma(0x1000, 0x4000, 0x0, 0x2000);
        let new_region = VirtMemoryRegion::new(VA::from_value(0x3000), 0x1000);

        let result = vma.shrink_to(new_region);

        assert!(matches!(result.kind, VMAreaKind::Anon));
    }
}
