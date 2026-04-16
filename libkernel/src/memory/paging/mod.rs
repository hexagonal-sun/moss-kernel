//! Architecture agnostic paging-related traits and types.

use super::{
    PAGE_SIZE,
    address::{PA, TPA, TVA, VA},
    region::PhysMemoryRegion,
};
use core::marker::PhantomData;
use permissions::PtePermissions;

pub mod permissions;
pub mod walk;

#[cfg(test)]
#[allow(missing_docs)]
pub mod test;

/// Trait for common behavior across different types of page table entries.
pub trait PageTableEntry: Sized + Copy + Clone {
    /// The raw pod-type used for the descriptor.
    type RawDescriptor: Sized + Copy + Clone;

    /// The raw value for an invalid (not present) descriptor.
    const INVALID: Self::RawDescriptor;

    /// The VA shift used for indexing at this descriptor level.
    const MAP_SHIFT: usize;

    /// Returns `true` if the entry is valid (i.e., not an Invalid/Fault entry).
    fn is_valid(self) -> bool;

    /// Returns the raw value of this page descriptor.
    fn as_raw(self) -> Self::RawDescriptor;

    /// Returns a representation of the page descriptor from a raw value.
    fn from_raw(v: Self::RawDescriptor) -> Self;

    /// Return a new invalid page descriptor.
    fn invalid() -> Self {
        Self::from_raw(Self::INVALID)
    }
}

/// Trait for descriptors that can point to a next-level table.
pub trait TableMapper: PageTableEntry {
    /// The type of page table this descriptor contains a PA for.
    type NextLevel: PgTable;

    /// Returns the physical address of the next-level table, if this descriptor
    /// is a table descriptor.
    fn next_table_address(self) -> Option<TPA<PgTableArray<Self::NextLevel>>>;

    /// Creates a new descriptor that points to the given next-level table.
    fn new_next_table(pa: TPA<PgTableArray<Self::NextLevel>>) -> Self;
}

/// A descriptor that maps a physical address at the page and block level.
pub trait PaMapper: PageTableEntry {
    /// The memory attribute type for this descriptor's architecture.
    type MemoryType: Copy;

    /// Constructs a new valid page descriptor that maps a physical address.
    fn new_map_pa(page_address: PA, memory_type: Self::MemoryType, perms: PtePermissions) -> Self;

    /// Whether a subsection of the region could be mapped via this type of
    /// page.
    fn could_map(region: PhysMemoryRegion, va: VA) -> bool;

    /// Return the mapped physical address.
    fn mapped_address(self) -> Option<PA>;
}

/// Trait representing a single level of the page table hierarchy.
///
/// Each implementor corresponds to a specific page table level, characterized
/// by its `SHIFT` value which determines the bits of the virtual address used
/// to index into the table.
///
/// # Associated Types
/// - `Descriptor`: The type representing an individual page table entry (PTE)
///   at this level.
///
/// # Constants
/// - `SHIFT`: The bit position to shift the virtual address to obtain the index
///   for this level.
/// - `DESCRIPTORS_PER_PAGE`: The number of PTE that are present in a single
///   page.
/// - `LEVEL_MASK`: The mask that should be applied after `SHIFT` to obtain the
///   descriptor index.
///
/// # Provided Methods
/// - `pg_index(va: VA) -> usize`: Calculate the index into the page table for
///   the given virtual address.
///
/// # Required Methods
/// - `get_desc(&self, va: VA) -> Self::Descriptor`: Retrieve the descriptor
///   (PTE) for the given virtual address.
/// - `get_desc_mut(&mut self, va: VA) -> &mut Self::Descriptor`: Get a mutable
///   reference to the descriptor, allowing updates.
pub trait PgTable: Clone + Copy {
    /// Number of page table descriptors that fit in a single 4 KiB page.
    const DESCRIPTORS_PER_PAGE: usize =
        PAGE_SIZE / core::mem::size_of::<<Self::Descriptor as PageTableEntry>::RawDescriptor>();

    /// Bitmask used to extract the page table index from a shifted virtual address.
    const LEVEL_MASK: usize = Self::DESCRIPTORS_PER_PAGE - 1;

    /// The descriptor (page table entry) type for this level.
    type Descriptor: PageTableEntry;

    /// Constructs this table handle from a typed virtual pointer to its backing array.
    fn from_ptr(ptr: TVA<PgTableArray<Self>>) -> Self;

    /// Returns the raw mutable pointer to the underlying descriptor array.
    fn to_raw_ptr(self) -> *mut u64;

    /// Compute the index into this page table from a virtual address.
    fn pg_index(va: VA) -> usize {
        (va.value() >> Self::Descriptor::MAP_SHIFT) & Self::LEVEL_MASK
    }

    /// Get the descriptor for a given virtual address.
    fn get_desc(self, va: VA) -> Self::Descriptor;

    /// Get the descriptor for a given index.
    fn get_idx(self, idx: usize) -> Self::Descriptor;

    /// Set the value of the descriptor for a particular VA.
    fn set_desc(self, va: VA, desc: Self::Descriptor, invalidator: &dyn TLBInvalidator);
}

/// A page-aligned array of raw page table entries for a given table level.
#[derive(Clone)]
#[repr(C, align(4096))]
pub struct PgTableArray<K: PgTable, const N: usize = 512> {
    pages: [<K::Descriptor as PageTableEntry>::RawDescriptor; N],
    _phantom: PhantomData<K>,
}

impl<K: PgTable, const N: usize> PgTableArray<K, N> {
    /// Creates a zeroed page table array (all entries invalid).
    pub const fn new() -> Self {
        Self {
            pages: [K::Descriptor::INVALID; N],
            _phantom: PhantomData,
        }
    }
}

impl<K: PgTable, const N: usize> Default for PgTableArray<K, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait for temporarily mapping and modifying a page table located at a
/// physical address.
///
/// During early boot, there are multiple mechanisms for accessing page table memory:
/// - Identity mapping (idmap): active very early when VA = PA
/// - Fixmap: a small, reserved region of virtual memory used to map arbitrary
///   PAs temporarily
/// - Page-offset (linear map/logical map): when VA = PA + offset, typically
///   used after MMU init
///
/// This trait abstracts over those mechanisms by providing a unified way to
/// safely access and mutate a page table given its physical address.
///
/// # Safety
/// This function is `unsafe` because the caller must ensure:
/// - The given physical address `pa` is valid and correctly aligned for type `T`.
/// - The contents at that physical address represent a valid page table of type `T`.
pub trait PageTableMapper {
    /// Map a physical address to a usable reference of the page table, run the
    /// closure, and unmap.
    ///
    /// # Safety
    /// This function is `unsafe` because the caller must ensure:
    /// - The given physical address `pa` is valid and correctly aligned for type `T`.
    /// - The contents at that physical address represent a valid page table of type `T`.
    unsafe fn with_page_table<T: PgTable, R>(
        &mut self,
        pa: TPA<PgTableArray<T>>,
        f: impl FnOnce(TVA<PgTableArray<T>>) -> R,
    ) -> crate::error::Result<R>;
}

// Conveneince trait.
pub(crate) trait TableMapperTable: PgTable<Descriptor: TableMapper> {
    /// Follows a descriptor to the next-level table if it's a valid table descriptor.
    // Dead code as only used in unit tests.
    #[allow(dead_code)]
    fn next_table_pa(
        self,
        va: VA,
    ) -> Option<TPA<PgTableArray<<Self::Descriptor as TableMapper>::NextLevel>>> {
        self.get_desc(va).next_table_address()
    }
}

impl<T: PgTable<Descriptor: TableMapper>> TableMapperTable for T {}

/// Trait for allocating new page tables during address space setup.
///
/// The page table walker uses this allocator to request fresh page tables
/// when needed (e.g., when creating new levels in the page table hierarchy).
///
/// # Responsibilities
/// - Return a valid, zeroed (or otherwise ready) page table physical address wrapped in `TPA<T>`.
/// - Ensure the allocated page table meets the alignment and size requirements of type `T`.
pub trait PageAllocator {
    /// Allocate a new page table of type `T` and return its physical address.
    ///
    /// # Errors
    /// Returns an error if allocation fails (e.g., out of memory).
    fn allocate_page_table<T: PgTable>(&mut self) -> crate::error::Result<TPA<PgTableArray<T>>>;
}

/// Trait for invalidating TLB entries after page table modifications.
pub trait TLBInvalidator {}

/// A no-op TLB invalidator used when invalidation is unnecessary.
pub struct NullTlbInvalidator {}

impl TLBInvalidator for NullTlbInvalidator {}
