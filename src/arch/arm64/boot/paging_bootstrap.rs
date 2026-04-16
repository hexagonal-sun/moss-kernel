use core::ptr;

use aarch64_cpu::asm::barrier;
use aarch64_cpu::registers::{MAIR_EL1, SCTLR_EL1, TCR_EL1, TTBR0_EL1, TTBR1_EL1};
use libkernel::arch::arm64::memory::pg_descriptors::MemoryType;
use libkernel::arch::arm64::memory::pg_tables::{
    L0Table, MapAttributes, MappingContext, map_range,
};
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::{AddressTranslator, IdentityTranslator, PA, TPA, TVA};
use libkernel::memory::paging::permissions::PtePermissions;
use libkernel::memory::paging::{
    NullTlbInvalidator, PageAllocator, PageTableMapper, PgTable, PgTableArray,
};
use libkernel::memory::region::PhysMemoryRegion;
use libkernel::memory::{PAGE_MASK, PAGE_SIZE};
use tock_registers::interfaces::{ReadWriteable, Writeable};

use crate::arch::arm64::memory::IMAGE_BASE;

use super::park_cpu;

const STATIC_PAGE_COUNT: usize = 128;
const MAX_FDT_SIZE: usize = 2 * 1024 * 1024;

unsafe extern "C" {
    static __image_start: u8;
    static __image_end: u8;
}

struct StaticPageAllocator {
    base: PA,
    allocated: usize,
}

impl StaticPageAllocator {
    fn from_phys_adr(addr: PA) -> Self {
        if addr.value() & PAGE_MASK != 0 {
            park_cpu();
        }

        Self {
            base: addr,
            allocated: 0,
        }
    }

    fn peek<T>(&self) -> TPA<T> {
        TPA::from_value(self.base.add_pages(self.allocated).value())
    }
}

impl PageAllocator for StaticPageAllocator {
    fn allocate_page_table<T: PgTable>(&mut self) -> Result<TPA<PgTableArray<T>>> {
        if self.allocated == STATIC_PAGE_COUNT {
            return Err(KernelError::NoMemory);
        }

        let ret = self.peek::<PgTableArray<T>>();

        unsafe {
            ptr::write_bytes(ret.as_ptr_mut().cast::<u8>(), 0, PAGE_SIZE);
        }

        self.allocated += 1;

        Ok(ret)
    }
}

struct KernelImageTranslator {}

impl<T> AddressTranslator<T> for KernelImageTranslator {
    fn virt_to_phys(_va: libkernel::memory::address::TVA<T>) -> TPA<T> {
        unreachable!("Should only be used to translate PA -> VA")
    }

    fn phys_to_virt(_pa: TPA<T>) -> libkernel::memory::address::TVA<T> {
        IMAGE_BASE.cast()
    }
}

struct IdmapTranslator {}

impl PageTableMapper for IdmapTranslator {
    unsafe fn with_page_table<T: PgTable, R>(
        &mut self,
        pa: TPA<PgTableArray<T>>,
        f: impl FnOnce(TVA<PgTableArray<T>>) -> R,
    ) -> Result<R> {
        Ok(f(pa.to_va::<IdentityTranslator>()))
    }
}

fn do_paging_bootstrap(static_pages: PA, image_addr: PA, fdt_addr: PA) -> Result<PA> {
    let mut bump_alloc = StaticPageAllocator::from_phys_adr(static_pages);

    // SAFETY: The MMU is currently disabled, accesses to physical ram will be
    // unrestricted.
    let idmap_l0 = bump_alloc.allocate_page_table::<L0Table>()?;

    // IDMAP kernel image.
    let image_size =
        unsafe { (&__image_end as *const u8).addr() - (&__image_start as *const u8).addr() };

    let kernel_range = PhysMemoryRegion::new(image_addr, image_size);

    let mut translator = IdmapTranslator {};
    let invalidator = NullTlbInvalidator {};

    let highmem_l0 = bump_alloc.allocate_page_table::<L0Table>()?;

    let mut bootstrap_ctx = MappingContext {
        allocator: &mut bump_alloc,
        mapper: &mut translator,
        invalidator: &invalidator,
    };

    map_range(
        idmap_l0,
        MapAttributes {
            phys: kernel_range,
            virt: kernel_range.map_via::<IdentityTranslator>(),
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rwx(false),
        },
        &mut bootstrap_ctx,
    )?;

    // IDMAP FDT
    let fdt_region = PhysMemoryRegion::new(fdt_addr, MAX_FDT_SIZE);
    map_range(
        idmap_l0,
        MapAttributes {
            phys: fdt_region,
            virt: fdt_region.map_via::<IdentityTranslator>(),
            mem_type: MemoryType::Normal,
            perms: PtePermissions::ro(false),
        },
        &mut bootstrap_ctx,
    )?;

    // TODO: Split out the permissions of the kernel image, such that the
    // appropriate permissions are set for each segment of the kernel image.
    map_range(
        highmem_l0,
        MapAttributes {
            phys: kernel_range,
            virt: kernel_range.map_via::<KernelImageTranslator>(),
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rwx(false),
        },
        &mut bootstrap_ctx,
    )?;

    enable_mmu(idmap_l0.to_untyped(), highmem_l0.to_untyped());

    Ok(highmem_l0.to_untyped())
}

#[unsafe(no_mangle)]
pub extern "C" fn enable_mmu(idmap_l0: PA, highmem_l0: PA) {
    TTBR0_EL1.set_baddr(idmap_l0.value() as u64); // Identity mapping

    TTBR1_EL1.set_baddr(highmem_l0.value() as u64); // Kernel high memory

    MAIR_EL1.write(
        MAIR_EL1::Attr0_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc
            + MAIR_EL1::Attr0_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc
            + MAIR_EL1::Attr1_Device::nonGathering_nonReordering_noEarlyWriteAck,
    );

    TCR_EL1.write(
        TCR_EL1::TBI1::Used +             // Top Byte Ignore for TTBR1
            TCR_EL1::IPS::Bits_40 +       // Physical address size = 40 bits
            TCR_EL1::TG1::KiB_4 +         // 4KB granule for TTBR1
            TCR_EL1::SH1::Inner +         // Inner shareable
            TCR_EL1::ORGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable +
            TCR_EL1::IRGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable +
            TCR_EL1::EPD1::EnableTTBR1Walks +
            TCR_EL1::T1SZ.val(16) +      // 48-bit VA for TTBR1

            TCR_EL1::TG0::KiB_4 +        // TTBR0 config (identity map region)
            TCR_EL1::SH0::Inner +
            TCR_EL1::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable +
            TCR_EL1::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable +
            TCR_EL1::A1::TTBR0 +
            TCR_EL1::EPD0::EnableTTBR0Walks +
            TCR_EL1::T0SZ.val(16), // 48-bit VA
    );

    barrier::dsb(barrier::ISHST);
    barrier::isb(barrier::SY);

    SCTLR_EL1.modify(SCTLR_EL1::M::Enable + SCTLR_EL1::C::Cacheable + SCTLR_EL1::I::Cacheable);

    barrier::isb(barrier::SY);
}

#[unsafe(no_mangle)]
pub extern "C" fn paging_bootstrap(static_pages: PA, image_phys_addr: PA, fdt_addr: PA) -> PA {
    let res = do_paging_bootstrap(static_pages, image_phys_addr, fdt_addr);

    if let Ok(addr) = res { addr } else { park_cpu() }
}
