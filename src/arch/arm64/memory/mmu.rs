use super::{MMIO_BASE, tlb::AllEl1TlbInvalidator};
use crate::sync::{OnceLock, SpinLock};
use libkernel::{
    arch::arm64::memory::{
        pg_descriptors::MemoryType,
        pg_tables::{L0Table, MapAttributes, MappingContext, map_range},
        pg_walk::translate as translate_va,
    },
    error::Result,
    memory::{
        address::{PA, TPA, VA},
        paging::{PgTableArray, permissions::PtePermissions},
        proc_vm::address_space::KernAddressSpace,
        region::{PhysMemoryRegion, VirtMemoryRegion},
    },
};
use page_allocator::PageTableAllocator;
use page_mapper::PageOffsetPgTableMapper;

pub mod page_allocator;
pub mod page_mapper;
pub mod smalloc_page_allocator;

pub static KERN_ADDR_SPC: OnceLock<SpinLock<Arm64KernelAddressSpace>> = OnceLock::new();

pub struct Arm64KernelAddressSpace {
    kernel_l0: TPA<PgTableArray<L0Table>>,
    mmio_ptr: VA,
}

impl Arm64KernelAddressSpace {
    fn do_map(&self, map_attrs: MapAttributes) -> Result<()> {
        let mut ctx = MappingContext {
            allocator: &mut PageTableAllocator::new(),
            mapper: &mut PageOffsetPgTableMapper {},
            invalidator: &AllEl1TlbInvalidator::new(),
        };

        map_range(self.kernel_l0, map_attrs, &mut ctx)
    }

    pub fn translate(&self, va: VA) -> Option<(PhysMemoryRegion, usize)> {
        translate_va(self.kernel_l0, va, &mut PageOffsetPgTableMapper {})
            .ok()
            .flatten()
            .map(|(region, offset, _)| (region, offset))
    }

    pub fn table_pa(&self) -> PA {
        self.kernel_l0.to_untyped()
    }
}

unsafe impl Send for Arm64KernelAddressSpace {}

impl KernAddressSpace for Arm64KernelAddressSpace {
    fn map_normal(
        &mut self,
        phys_range: PhysMemoryRegion,
        virt_range: VirtMemoryRegion,
        perms: PtePermissions,
    ) -> Result<()> {
        self.do_map(MapAttributes {
            phys: phys_range,
            virt: virt_range,
            mem_type: MemoryType::Normal,
            perms,
        })
    }

    fn map_mmio(&mut self, phys_range: PhysMemoryRegion) -> Result<VA> {
        let phys_mappable_region = phys_range.to_mappable_region();
        let base_va = self.mmio_ptr;

        let virt_range = VirtMemoryRegion::new(base_va, phys_mappable_region.region().size());

        self.do_map(MapAttributes {
            phys: phys_mappable_region.region(),
            virt: virt_range,
            mem_type: MemoryType::Device,
            perms: PtePermissions::rw(false),
        })?;

        self.mmio_ptr =
            VA::from_value(self.mmio_ptr.value() + phys_mappable_region.region().size());

        Ok(VA::from_value(
            base_va.value() + phys_mappable_region.offset(),
        ))
    }
}

pub fn setup_kern_addr_space(pa: TPA<PgTableArray<L0Table>>) -> Result<()> {
    let addr_space = SpinLock::new(Arm64KernelAddressSpace {
        kernel_l0: pa,
        mmio_ptr: MMIO_BASE,
    });

    KERN_ADDR_SPC
        .set(addr_space)
        .map_err(|_| libkernel::error::KernelError::InUse)
}
