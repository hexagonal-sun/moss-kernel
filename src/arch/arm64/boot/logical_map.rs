use crate::memory::{INITAL_ALLOCATOR, PageOffsetTranslator};

use super::super::memory::{
    fixmap::{FIXMAPS, Fixmap},
    mmu::smalloc_page_allocator::SmallocPageAlloc,
    tlb::AllEl1TlbInvalidator,
};
use libkernel::{
    arch::arm64::memory::{
        pg_descriptors::MemoryType,
        pg_tables::{L0Table, MapAttributes, MappingContext, map_range},
    },
    error::Result,
    memory::{
        address::{TPA, TVA},
        paging::{PageTableMapper, PgTable, PgTableArray, permissions::PtePermissions},
    },
};

pub struct FixmapMapper<'a> {
    pub fixmaps: &'a mut Fixmap,
}

impl PageTableMapper for FixmapMapper<'_> {
    unsafe fn with_page_table<T: PgTable, R>(
        &mut self,
        pa: TPA<PgTableArray<T>>,
        f: impl FnOnce(TVA<PgTableArray<T>>) -> R,
    ) -> Result<R> {
        let guard = self.fixmaps.temp_remap_page_table(pa)?;

        // SAFETY: The guard will live for the lifetime of the closure.
        Ok(f(unsafe { guard.get_va() }))
    }
}

pub fn setup_logical_map(pgtbl_base: TPA<PgTableArray<L0Table>>) -> Result<()> {
    let mut fixmaps = FIXMAPS.lock_save_irq();
    let mut alloc = INITAL_ALLOCATOR.lock_save_irq();
    let alloc = alloc.as_mut().unwrap();
    let mem_list = alloc.get_memory_list();
    let mut mapper = FixmapMapper {
        fixmaps: &mut fixmaps,
    };
    let mut pg_alloc = SmallocPageAlloc::new(alloc);

    let mut ctx = MappingContext {
        allocator: &mut pg_alloc,
        mapper: &mut mapper,
        invalidator: &AllEl1TlbInvalidator::new(),
    };

    for mem_region in mem_list.iter() {
        let map_attrs = MapAttributes {
            phys: mem_region,
            virt: mem_region.map_via::<PageOffsetTranslator>(),
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(false),
        };

        map_range(pgtbl_base, map_attrs, &mut ctx)?;
    }

    Ok(())
}
