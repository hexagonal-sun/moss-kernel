use super::address::{AddressTranslator, TPA, TVA};
use crate::VirtualMemory;
use core::marker::PhantomData;

pub struct PageOffsetTranslator<VM: VirtualMemory> {
    _phantom: PhantomData<VM>,
}

unsafe impl<VM: VirtualMemory> Send for PageOffsetTranslator<VM> {}
unsafe impl<VM: VirtualMemory> Sync for PageOffsetTranslator<VM> {}

impl<T, VM: VirtualMemory> AddressTranslator<T> for PageOffsetTranslator<VM> {
    fn virt_to_phys(va: TVA<T>) -> TPA<T> {
        let mut v = va.value();

        v -= VM::PAGE_OFFSET;

        TPA::from_value(v)
    }

    fn phys_to_virt(pa: TPA<T>) -> TVA<T> {
        let mut v = pa.value();

        v += VM::PAGE_OFFSET;

        TVA::from_value(v)
    }
}
