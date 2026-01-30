use crate::memory::PAGE_ALLOC;

use super::{
    mmu::{page_allocator::PageTableAllocator, page_mapper::PageOffsetPgTableMapper},
    tlb::AllEl0TlbInvalidator,
};
use aarch64_cpu::{
    asm::barrier::{ISH, SY, dsb, isb},
    registers::{ReadWriteable, TCR_EL1, TTBR0_EL1},
};
use alloc::vec::Vec;
use libkernel::{
    PageInfo, UserAddressSpace,
    arch::arm64::memory::{
        pg_descriptors::{L3Descriptor, MemoryType, PaMapper, PageTableEntry},
        pg_tables::{
            L0Table, MapAttributes, MappingContext, PageAllocator, PgTableArray, map_range,
        },
        pg_tear_down::tear_down_address_space,
        pg_walk::{WalkContext, get_pte, walk_and_modify_region},
    },
    error::{KernelError, MapError, Result},
    memory::{
        PAGE_SIZE,
        address::{TPA, VA},
        page::PageFrame,
        permissions::PtePermissions,
        region::{PhysMemoryRegion, VirtMemoryRegion},
    },
};
use log::warn;

pub struct Arm64ProcessAddressSpace {
    l0_table: TPA<PgTableArray<L0Table>>,
}

unsafe impl Send for Arm64ProcessAddressSpace {}
unsafe impl Sync for Arm64ProcessAddressSpace {}

impl UserAddressSpace for Arm64ProcessAddressSpace {
    fn new() -> Result<Self>
    where
        Self: Sized,
    {
        let l0_table = PageTableAllocator::new().allocate_page_table()?;

        Ok(Self { l0_table })
    }

    fn activate(&self) {
        let _invalidator = AllEl0TlbInvalidator;
        TTBR0_EL1.set_baddr(self.l0_table.value() as u64);
        dsb(ISH);
        TCR_EL1.modify(TCR_EL1::EPD0::EnableTTBR0Walks);
        isb(SY);
    }

    fn deactivate(&self) {
        let _invalidator = AllEl0TlbInvalidator;
        TCR_EL1.modify(TCR_EL1::EPD0::DisableTTBR0Walks);
        isb(SY);
    }

    fn map_page(&mut self, page: PageFrame, va: VA, perms: PtePermissions) -> Result<()> {
        let mut ctx = MappingContext {
            allocator: &mut PageTableAllocator::new(),
            mapper: &mut PageOffsetPgTableMapper {},
            invalidator: &AllEl0TlbInvalidator::new(),
        };

        map_range(
            self.l0_table,
            MapAttributes {
                phys: page.as_phys_range(),
                virt: VirtMemoryRegion::new(va, PAGE_SIZE),
                mem_type: MemoryType::Normal,
                perms,
            },
            &mut ctx,
        )
    }

    fn unmap(&mut self, _va: VA) -> Result<PageFrame> {
        todo!()
    }

    fn protect_range(&mut self, va_range: VirtMemoryRegion, perms: PtePermissions) -> Result<()> {
        let mut walk_ctx = WalkContext {
            mapper: &mut PageOffsetPgTableMapper {},
            invalidator: &AllEl0TlbInvalidator::new(),
        };

        walk_and_modify_region(self.l0_table, va_range, &mut walk_ctx, |_, desc| {
            match (perms.is_execute(), perms.is_read(), perms.is_write()) {
                (false, false, false) => desc.mark_as_swapped(),
                _ => desc.set_permissions(perms),
            }
        })
    }

    fn unmap_range(&mut self, va_range: VirtMemoryRegion) -> Result<Vec<PageFrame>> {
        let mut walk_ctx = WalkContext {
            mapper: &mut PageOffsetPgTableMapper {},
            invalidator: &AllEl0TlbInvalidator::new(),
        };
        let mut claimed_pages = Vec::new();

        walk_and_modify_region(self.l0_table, va_range, &mut walk_ctx, |_, desc| {
            if let Some(addr) = desc.mapped_address() {
                claimed_pages.push(addr.to_pfn());
            }

            L3Descriptor::invalid()
        })?;

        Ok(claimed_pages)
    }

    fn remap(&mut self, va: VA, new_page: PageFrame, perms: PtePermissions) -> Result<PageFrame> {
        let mut walk_ctx = WalkContext {
            mapper: &mut PageOffsetPgTableMapper {},
            invalidator: &AllEl0TlbInvalidator::new(),
        };

        let mut old_pte = None;

        walk_and_modify_region(self.l0_table, va.page_region(), &mut walk_ctx, |_, pte| {
            old_pte = Some(pte);
            L3Descriptor::new_map_pa(new_page.pa(), MemoryType::Normal, perms)
        })?;

        old_pte
            .and_then(|pte| pte.mapped_address())
            .map(|a| a.to_pfn())
            .ok_or(KernelError::MappingError(MapError::NotL3Mapped))
    }

    fn translate(&self, va: VA) -> Option<PageInfo> {
        let pte = get_pte(
            self.l0_table,
            va.page_aligned(),
            &mut PageOffsetPgTableMapper {},
        )
        .unwrap()?;

        Some(PageInfo {
            pfn: pte.mapped_address()?.to_pfn(),
            perms: pte.permissions()?,
        })
    }

    fn protect_and_clone_region(
        &mut self,
        region: VirtMemoryRegion,
        other: &mut Self,
        new_perms: PtePermissions,
    ) -> Result<()>
    where
        Self: Sized,
    {
        let mut walk_ctx = WalkContext {
            mapper: &mut PageOffsetPgTableMapper {},
            invalidator: &AllEl0TlbInvalidator::new(),
        };

        walk_and_modify_region(self.l0_table, region, &mut walk_ctx, |va, pgd| {
            if let Some(addr) = pgd.mapped_address() {
                let page_region = PhysMemoryRegion::new(addr, PAGE_SIZE);

                // SAFETY: This is safe since the page will have allocated when
                // handling faults.
                let alloc1 = unsafe { PAGE_ALLOC.get().unwrap().alloc_from_region(page_region) };

                // Increase ref count.
                alloc1.clone().leak();
                alloc1.leak();

                let mut ctx = MappingContext {
                    allocator: &mut PageTableAllocator::new(),
                    mapper: &mut PageOffsetPgTableMapper {},
                    invalidator: &AllEl0TlbInvalidator::new(),
                };

                map_range(
                    other.l0_table,
                    MapAttributes {
                        phys: PhysMemoryRegion::new(addr, PAGE_SIZE),
                        virt: VirtMemoryRegion::new(va, PAGE_SIZE),
                        mem_type: MemoryType::Normal,
                        perms: new_perms,
                    },
                    &mut ctx,
                )
                .unwrap();

                pgd.set_permissions(new_perms)
            } else {
                pgd
            }
        })
    }
}

impl Drop for Arm64ProcessAddressSpace {
    fn drop(&mut self) {
        let mut walk_ctx = WalkContext {
            mapper: &mut PageOffsetPgTableMapper {},
            invalidator: &AllEl0TlbInvalidator::new(),
        };

        if tear_down_address_space(self.l0_table, &mut walk_ctx, |addr| unsafe {
            PAGE_ALLOC
                .get()
                .unwrap()
                .alloc_from_region(addr.to_pfn().as_phys_range());
        })
        .is_err()
        {
            warn!("Address space tear down failed.  Probable memory leakage!");
        }
    }
}
