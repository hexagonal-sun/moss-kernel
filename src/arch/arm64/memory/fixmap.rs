use super::{FIXMAP_BASE, tlb::AllEl1TlbInvalidator};
use crate::{arch::arm64::fdt::MAX_FDT_SZ, ksym_pa, sync::SpinLock};
use core::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};
use libkernel::{
    arch::arm64::memory::{
        pg_descriptors::{
            L0Descriptor, L1Descriptor, L2Descriptor, L3Descriptor, MemoryType, PaMapper,
            PageTableEntry, TableMapper,
        },
        pg_tables::{L0Table, L1Table, L2Table, L3Table, PgTable, PgTableArray},
    },
    error::{KernelError, Result},
    memory::{
        PAGE_SIZE,
        address::{IdentityTranslator, TPA, TVA, VA},
        permissions::PtePermissions,
        region::PhysMemoryRegion,
    },
};

pub struct TempFixmapGuard<T> {
    fixmap: *mut Fixmap,
    va: TVA<T>,
}

impl<T> TempFixmapGuard<T> {
    /// Get the VA associated with this temp fixmap.
    ///
    /// SAFETY: The returned VA is not tied back to the lifetime of the guard.
    /// Thefeore, care *must* be taken that it is not used after the guard has
    /// gone out of scope.
    pub unsafe fn get_va(&self) -> TVA<T> {
        self.va
    }
}

impl<T> Deref for TempFixmapGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.va.as_ptr().cast::<T>().as_ref().unwrap() }
    }
}

impl<T> DerefMut for TempFixmapGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.va.as_ptr_mut().cast::<T>().as_mut().unwrap() }
    }
}

impl<T> Drop for TempFixmapGuard<T> {
    fn drop(&mut self) {
        unsafe {
            let fixmap = &mut *self.fixmap;
            fixmap.unmap_temp_page();
        }
    }
}

#[derive(Clone, Copy)]
#[repr(usize)]
enum FixmapSlot {
    DtbStart = 0,
    _DtbEnd = MAX_FDT_SZ / PAGE_SIZE, // 2MiB max DTB size,
    PgTableTmp,
}

pub struct Fixmap {
    l1: PgTableArray<L1Table>,
    l2: PgTableArray<L2Table>,
    l3: [PgTableArray<L3Table>; 2],
}

unsafe impl Send for Fixmap {}
unsafe impl Sync for Fixmap {}

pub static FIXMAPS: SpinLock<Fixmap> = SpinLock::new(Fixmap::new());

impl Fixmap {
    pub const fn new() -> Self {
        Self {
            l1: PgTableArray::new(),
            l2: PgTableArray::new(),
            l3: [const { PgTableArray::new() }; 2],
        }
    }

    pub fn setup_fixmaps(&mut self, l0_base: TPA<PgTableArray<L0Table>>) {
        let l0_table = L0Table::from_ptr(l0_base.to_va::<IdentityTranslator>());
        let invalidator = AllEl1TlbInvalidator::new();

        L1Table::from_ptr(TVA::from_ptr(&mut self.l1 as *mut _)).set_desc(
            FIXMAP_BASE,
            L1Descriptor::new_next_table(ksym_pa!(self.l2)),
            &invalidator,
        );

        L2Table::from_ptr(TVA::from_ptr(&mut self.l2 as *mut _)).set_desc(
            FIXMAP_BASE,
            L2Descriptor::new_next_table(ksym_pa!(self.l3[0])),
            &invalidator,
        );

        L2Table::from_ptr(TVA::from_ptr(&mut self.l2 as *mut _)).set_desc(
            VA::from_value(FIXMAP_BASE.value() + (1 << L2Table::SHIFT)),
            L2Descriptor::new_next_table(ksym_pa!(self.l3[1])),
            &invalidator,
        );

        l0_table.set_desc(
            FIXMAP_BASE,
            L0Descriptor::new_next_table(ksym_pa!(self.l1)),
            &invalidator,
        );
    }

    /// Remap the FDT via the fixmaps.
    ///
    /// Unsafe as this will attempt to read the FDT size from the given address,
    /// therefore ID mappings must be in place to read from the PA.
    pub unsafe fn remap_fdt(&mut self, fdt_ptr: TPA<u8>) -> Result<VA> {
        let fdt =
            unsafe { fdt_parser::Fdt::from_ptr(NonNull::new_unchecked(fdt_ptr.as_ptr_mut())) }
                .map_err(|_| KernelError::InvalidValue)?;

        let sz = fdt.total_size();

        if sz > MAX_FDT_SZ {
            return Err(KernelError::TooLarge);
        }

        let mut phys_region = PhysMemoryRegion::new(fdt_ptr.to_untyped(), sz);
        let mut va = FIXMAP_BASE;
        let invalidator = AllEl1TlbInvalidator::new();

        while phys_region.size() > 0 {
            L3Table::from_ptr(TVA::from_ptr_mut(&mut self.l3[0] as *mut _)).set_desc(
                va,
                L3Descriptor::new_map_pa(
                    phys_region.start_address(),
                    MemoryType::Normal,
                    PtePermissions::ro(false),
                ),
                &invalidator,
            );

            phys_region = phys_region.add_pages(1);
            va = va.add_pages(1);
        }

        Ok(Self::va_for_slot(FixmapSlot::DtbStart))
    }

    pub fn temp_remap_page_table<T: PgTable>(
        &mut self,
        pa: TPA<PgTableArray<T>>,
    ) -> Result<TempFixmapGuard<PgTableArray<T>>> {
        let va = Self::va_for_slot(FixmapSlot::PgTableTmp);
        let invalidator = AllEl1TlbInvalidator::new();

        L3Table::from_ptr(TVA::from_ptr_mut(&mut self.l3[1] as *mut _)).set_desc(
            va,
            L3Descriptor::new_map_pa(
                pa.to_untyped(),
                MemoryType::Normal,
                PtePermissions::rw(false),
            ),
            &invalidator,
        );

        Ok(TempFixmapGuard {
            fixmap: self as *mut _,
            va: va.cast(),
        })
    }

    fn unmap_temp_page(&mut self) {
        let va = Self::va_for_slot(FixmapSlot::PgTableTmp);
        let invalidator = AllEl1TlbInvalidator::new();

        L3Table::from_ptr(TVA::from_ptr_mut(&mut self.l3[1] as *mut _)).set_desc(
            va,
            L3Descriptor::invalid(),
            &invalidator,
        );
    }

    fn va_for_slot(slot: FixmapSlot) -> VA {
        TVA::from_value(FIXMAP_BASE.value() + (slot as usize * PAGE_SIZE))
    }
}
