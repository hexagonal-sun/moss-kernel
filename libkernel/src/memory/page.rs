use super::{
    PAGE_SHIFT,
    address::PA,
    region::PhysMemoryRegion,
};
use crate::memory::PAGE_SIZE;
use core::fmt::Display;

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
    pub fn from_pfn(n: usize) -> Self {
        Self { n }
    }

    pub fn pa(&self) -> PA {
        PA::from_value(self.n << PAGE_SHIFT)
    }

    pub fn as_phys_range(&self) -> PhysMemoryRegion {
        PhysMemoryRegion::new(self.pa(), PAGE_SIZE)
    }

    pub fn value(&self) -> usize {
        self.n
    }

    #[cfg(feature = "alloc")]
    pub(crate) fn buddy(self, order: usize) -> Self {
        Self {
            n: self.n ^ (1 << order),
        }
    }

    #[must_use]
    pub fn add_pages(self, n: usize) -> Self {
        Self { n: self.n + n }
    }
}
