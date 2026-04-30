//! Page-offset arithmetic helpers.

pub(crate) use crate::memory::address::{AddressTranslator, TPA, TVA};

/// Translates between physical and virtual addresses using a fixed page-offset mapping.
pub struct PageOffsetTranslator<const OFFSET: usize>;

unsafe impl<const OFFSET: usize> Send for PageOffsetTranslator<OFFSET> {}
unsafe impl<const OFFSET: usize> Sync for PageOffsetTranslator<OFFSET> {}

impl<T, const OFFSET: usize> AddressTranslator<T> for PageOffsetTranslator<OFFSET> {
    fn virt_to_phys(va: TVA<T>) -> TPA<T> {
        let mut v = va.value();

        v -= OFFSET;

        TPA::from_value(v)
    }

    fn phys_to_virt(pa: TPA<T>) -> TVA<T> {
        let mut v = pa.value();

        v += OFFSET;

        TVA::from_value(v)
    }
}
