//! `address` module: Type-safe handling of virtual and physical addresses.
//!
//! This module defines strongly-typed address representations for both physical
//! and virtual memory. It provides abstractions to ensure correct usage and
//! translation between address spaces, as well as alignment and page-based
//! operations.
//!
//! ## Key Features
//!
//! - Typed Addresses: Differentiates between physical and virtual addresses at
//!   compile time.
//! - Generic Address Types: Supports pointer-type phantom typing for safety and
//!   clarity.
//! - Page Alignment & Arithmetic: Includes utility methods for page alignment
//!   and manipulation.
//! - Translators: Provides an `AddressTranslator` trait for converting between
//!   physical and virtual addresses.
//!
//! ## Safety
//! Raw pointer access to physical addresses (`TPA<T>::as_ptr`, etc.) is marked
//! `unsafe` and assumes the caller ensures correctness (e.g., MMU off/ID
//! mappings).
//!
//! ## Example
//! ```rust
//! use libkernel::memory::address::*;
//!
//! let pa: PA = PA::from_value(0x1000);
//! assert!(pa.is_page_aligned());
//!
//! let va = pa.to_va::<IdentityTranslator>();
//! let pa2 = va.to_pa::<IdentityTranslator>();
//! assert_eq!(pa, pa2);
//! ```

use super::{PAGE_MASK, PAGE_SHIFT, PAGE_SIZE, page::PageFrame, region::MemoryRegion};
use core::{
    fmt::{self, Debug, Display},
    marker::PhantomData,
};

mod sealed {
    /// Sealed trait to prevent external implementations of `MemKind`.
    pub trait Sealed {}
}

/// Marker trait for kinds of memory addresses (virtual or physical).
///
/// Implemented only for `Virtual` and `Physical`.
pub trait MemKind: sealed::Sealed + Ord + Clone + Copy + PartialEq + Eq {}

/// Marker for virtual memory address type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Virtual;

/// Marker for physical memory address type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Physical;

/// Marker for user memory address type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct User;

impl sealed::Sealed for Virtual {}
impl sealed::Sealed for Physical {}
impl sealed::Sealed for User {}

impl MemKind for Virtual {}
impl MemKind for Physical {}
impl MemKind for User {}

/// A memory address with a kind (`Virtual`, `Physical`, or `User`) and an
/// associated data type.
///
/// The `T` phantom type is useful for distinguishing address purposes (e.g.,
/// different hardware devices).
#[derive(PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub struct Address<K: MemKind, T> {
    inner: usize,
    _phantom: PhantomData<K>,
    _phantom_type: PhantomData<T>,
}

impl<K: MemKind, T> Clone for Address<K, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: MemKind, T> Copy for Address<K, T> {}

impl<K: MemKind, T> Debug for Address<K, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:04x}", self.inner)
    }
}

impl<K: MemKind, T> Address<K, T> {
    /// Construct an address from a raw usize value.
    pub const fn from_value(addr: usize) -> Self {
        Self {
            inner: addr,
            _phantom: PhantomData,
            _phantom_type: PhantomData,
        }
    }

    /// Return the underlying raw address value.
    pub const fn value(self) -> usize {
        self.inner
    }

    /// Check if the address is aligned to the system's page size.
    pub const fn is_page_aligned(self) -> bool {
        self.inner & PAGE_MASK == 0
    }

    /// Add `count` pages to the address.
    #[must_use]
    pub const fn add_pages(self, count: usize) -> Self {
        Self::from_value(self.inner + (PAGE_SIZE * count))
    }

    /// Return an address aligned down to `align` (must be a power of two).
    #[must_use]
    pub const fn align(self, align: usize) -> Self {
        assert!(align.is_power_of_two());
        Self::from_value(self.inner & !(align - 1))
    }

    /// Return an address aligned down to the next page boundary.
    #[must_use]
    pub const fn page_aligned(self) -> Self {
        Self::from_value(self.inner & !PAGE_MASK)
    }

    /// Return an address aligned up to `align` (must be a power of two).
    #[must_use]
    pub const fn align_up(self, align: usize) -> Self {
        assert!(align.is_power_of_two());
        Self::from_value((self.inner + (align - 1)) & !(align - 1))
    }

    /// Get the offset of the address within its page.
    pub fn page_offset(self) -> usize {
        self.inner & PAGE_MASK
    }

    pub const fn null() -> Self {
        Self::from_value(0)
    }

    #[must_use]
    pub const fn add_bytes(self, n: usize) -> Self {
        Self::from_value(self.value() + n)
    }

    #[must_use]
    pub fn sub_bytes(self, n: usize) -> Self {
        Self::from_value(self.value() - n)
    }

    pub fn is_null(self) -> bool {
        self.inner == 0
    }
}

impl<K: MemKind, T: Sized> Address<K, T> {
    #[must_use]
    /// Increments the pointer by the number of T *objects* n.
    pub fn add_objs(self, n: usize) -> Self {
        Self::from_value(self.value() + core::mem::size_of::<T>() * n)
    }

    /// Increments the pointer by the number of T *objects* n.
    pub fn sub_objs(self, n: usize) -> Self {
        Self::from_value(self.value() - core::mem::size_of::<T>() * n)
    }
}

/// A typed physical address.
pub type TPA<T> = Address<Physical, T>;
/// A typed virtual address.
pub type TVA<T> = Address<Virtual, T>;
/// A typed user address.
pub type TUA<T> = Address<User, T>;

/// An untyped physical address.
pub type PA = Address<Physical, ()>;
/// An untyped virtual address.
pub type VA = Address<Virtual, ()>;
/// An untyped user address.
pub type UA = Address<User, ()>;

impl<T> TPA<T> {
    /// Convert to a raw const pointer.
    ///
    /// # Safety
    /// Caller must ensure memory is accessible and valid for read.
    pub unsafe fn as_ptr(self) -> *const T {
        self.value() as _
    }

    /// Convert to a raw mutable pointer.
    ///
    /// # Safety
    /// Caller must ensure memory is accessible and valid for read/write.
    pub unsafe fn as_ptr_mut(self) -> *mut T {
        self.value() as _
    }

    /// Convert to an untyped physical address.
    pub fn to_untyped(self) -> PA {
        PA::from_value(self.value())
    }

    /// Convert to a virtual address using a translator.
    pub fn to_va<A: AddressTranslator<T>>(self) -> TVA<T> {
        A::phys_to_virt(self)
    }
}

impl<T> Display for TPA<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Px{:08x}", self.inner)
    }
}

impl<T> TVA<T> {
    /// Convert to a raw const pointer.
    pub fn as_ptr(self) -> *const T {
        self.value() as _
    }

    /// Convert to a raw mutable pointer.
    pub fn as_ptr_mut(self) -> *mut T {
        self.value() as _
    }

    /// Convert a raw pointer to a TVA.
    pub fn from_ptr(ptr: *const T) -> TVA<T> {
        Self::from_value(ptr.addr())
    }

    /// Convert a raw mutable pointer to a TVA.
    pub fn from_ptr_mut(ptr: *mut T) -> TVA<T> {
        Self::from_value(ptr.addr())
    }

    /// Convert to an untyped virtual address.
    pub fn to_untyped(self) -> VA {
        VA::from_value(self.value())
    }

    /// Convert to a physical address using a translator.
    pub fn to_pa<A: AddressTranslator<T>>(self) -> TPA<T> {
        A::virt_to_phys(self)
    }
}

impl<T> TUA<T> {
    /// Convert to an untyped user address.
    pub fn to_untyped(self) -> UA {
        UA::from_value(self.value())
    }
}

impl UA {
    /// Cast to a typed user address.
    pub fn cast<T>(self) -> TUA<T> {
        TUA::from_value(self.value())
    }
}

impl VA {
    /// Cast to a typed virtual address.
    pub fn cast<T>(self) -> TVA<T> {
        TVA::from_value(self.value())
    }

    /// Return a region representing the page to which this address belongs.
    pub fn page_region(self) -> MemoryRegion<Virtual> {
        MemoryRegion::new(self.page_aligned().cast(), PAGE_SIZE)
    }
}

impl PA {
    /// Cast to a typed physical address.
    pub fn cast<T>(self) -> TPA<T> {
        TPA::from_value(self.value())
    }

    pub fn to_pfn(&self) -> PageFrame {
        PageFrame::from_pfn(self.inner >> PAGE_SHIFT)
    }
}

/// Trait for translating between physical and virtual addresses.
pub trait AddressTranslator<T>: 'static + Send + Sync {
    fn virt_to_phys(va: TVA<T>) -> TPA<T>;
    fn phys_to_virt(pa: TPA<T>) -> TVA<T>;
}

/// A simple address translator that performs identity mapping.
pub struct IdentityTranslator {}

impl<T> AddressTranslator<T> for IdentityTranslator {
    fn virt_to_phys(va: TVA<T>) -> TPA<T> {
        TPA::from_value(va.value())
    }

    fn phys_to_virt(pa: TPA<T>) -> TVA<T> {
        TVA::from_value(pa.value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ADDR: usize = 0x1000;
    const NON_ALIGNED_ADDR: usize = 0x1234;

    #[test]
    fn test_va_creation_and_value() {
        let va = VA::from_value(TEST_ADDR);
        assert_eq!(va.value(), TEST_ADDR);
    }

    #[test]
    fn test_pa_creation_and_value() {
        let pa = PA::from_value(TEST_ADDR);
        assert_eq!(pa.value(), TEST_ADDR);
    }

    #[test]
    fn test_page_alignment() {
        let aligned_va = VA::from_value(0x2000);
        let unaligned_va = VA::from_value(0x2001);

        assert!(aligned_va.is_page_aligned());
        assert!(!unaligned_va.is_page_aligned());
    }

    #[test]
    fn test_add_pages() {
        let base = VA::from_value(0x1000);
        let added = base.add_pages(2);
        assert_eq!(added.value(), 0x1000 + 2 * PAGE_SIZE);
    }

    #[test]
    fn test_align_down() {
        let addr = VA::from_value(NON_ALIGNED_ADDR);
        let aligned = addr.align(0x1000);
        assert_eq!(aligned.value(), NON_ALIGNED_ADDR & !0xfff);
    }

    #[test]
    fn test_align_up() {
        let addr = VA::from_value(NON_ALIGNED_ADDR);
        let aligned = addr.align_up(0x1000);
        let expected = (NON_ALIGNED_ADDR + 0xfff) & !0xfff;
        assert_eq!(aligned.value(), expected);
    }

    #[test]
    fn test_identity_translation_va_to_pa() {
        let va = VA::from_value(TEST_ADDR);
        let pa = va.to_pa::<IdentityTranslator>();
        assert_eq!(pa.value(), TEST_ADDR);
    }

    #[test]
    fn test_identity_translation_pa_to_va() {
        let pa = PA::from_value(TEST_ADDR);
        let va = pa.to_va::<IdentityTranslator>();
        assert_eq!(va.value(), TEST_ADDR);
    }

    #[test]
    fn test_va_pointer_conversion() {
        let va = VA::from_value(0x1000);
        let ptr: *const u8 = va.as_ptr() as *const _;
        assert_eq!(ptr as usize, 0x1000);
    }

    #[test]
    fn test_va_mut_pointer_conversion() {
        let va = VA::from_value(0x1000);
        let ptr: *mut u8 = va.as_ptr_mut() as *mut _;
        assert_eq!(ptr as usize, 0x1000);
    }
}
