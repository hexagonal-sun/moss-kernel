//! Page frame numbers.
//!
//! A [`PageFrame`] is a lightweight handle for a physical page, identified by
//! its page frame number (PFN).

use super::{PAGE_SHIFT, address::PA, region::PhysMemoryRegion};
use crate::memory::PAGE_SIZE;
use core::fmt::Display;

/// A page frame number (PFN) — an index into physical memory in units of
/// [`PAGE_SIZE`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct PageFrame {
    n: usize,
}

impl Display for PageFrame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.n.fmt(f)
    }
}

impl PageFrame {
    /// Creates a `PageFrame` from a raw page frame number.
    pub fn from_pfn(n: usize) -> Self {
        Self { n }
    }

    /// Returns the physical address of the start of this page frame.
    pub fn pa(&self) -> PA {
        PA::from_value(self.n << PAGE_SHIFT)
    }

    /// Returns this page frame as a single-page physical memory region.
    pub fn as_phys_range(&self) -> PhysMemoryRegion {
        PhysMemoryRegion::new(self.pa(), PAGE_SIZE)
    }

    /// Returns the raw page frame number.
    pub fn value(&self) -> usize {
        self.n
    }

    #[cfg(feature = "alloc")]
    pub(crate) fn buddy(self, order: usize) -> Self {
        Self {
            n: self.n ^ (1 << order),
        }
    }

    /// Returns a new `PageFrame` offset by `n` pages.
    #[must_use]
    pub fn add_pages(self, n: usize) -> Self {
        Self { n: self.n + n }
    }
}
