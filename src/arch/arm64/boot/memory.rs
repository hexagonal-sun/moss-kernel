use crate::arch::arm64::memory::{
    HEAP_ALLOCATOR,
    mmu::{page_mapper::PageOffsetPgTableMapper, smalloc_page_allocator::SmallocPageAlloc},
    set_kimage_start,
    tlb::AllEl1TlbInvalidator,
};
use crate::memory::INITAL_ALLOCATOR;
use core::ptr::NonNull;
use libkernel::{
    arch::arm64::memory::{
        pg_descriptors::MemoryType,
        pg_tables::{L0Table, MapAttributes, MappingContext, PgTableArray, map_range},
    },
    error::{KernelError, Result},
    memory::{
        PAGE_SIZE,
        address::{PA, TPA, VA},
        permissions::PtePermissions,
        region::{PhysMemoryRegion, VirtMemoryRegion},
    },
};
use log::info;

const KERNEL_STACK_SZ: usize = 64 * 1024; // 32 KiB
pub const KERNEL_STACK_PG_ORDER: usize = (KERNEL_STACK_SZ / PAGE_SIZE).ilog2() as usize;

const KERNEL_HEAP_SZ: usize = 64 * 1024 * 1024; // 64 MiB

pub fn setup_allocator(dtb_ptr: TPA<u8>, image_start: PA, image_end: PA) -> Result<()> {
    let dt = unsafe { fdt_parser::Fdt::from_ptr(NonNull::new_unchecked(dtb_ptr.as_ptr_mut())) }
        .map_err(|_| KernelError::InvalidValue)?;

    let mut alloc = INITAL_ALLOCATOR.lock_save_irq();
    let alloc = alloc.as_mut().unwrap();

    dt.memory().try_for_each(|mem| -> Result<()> {
        mem.regions().try_for_each(|region| -> Result<()> {
            let start_addr = PA::from_value(region.address.addr());

            info!(
                "Adding memory region from FDT {start_addr} (0x{:x} bytes)",
                region.size
            );

            alloc.add_memory(PhysMemoryRegion::new(start_addr, region.size))?;

            Ok(())
        })
    })?;

    // If we couldn't find any memory regions, we cannot continue.
    if alloc.base_ram_base_address().is_none() {
        return Err(KernelError::NoMemory);
    }

    dt.memory_reservation_block()
        .try_for_each(|res| -> Result<()> {
            let start_addr = PA::from_value(res.address.addr());

            info!(
                "Adding reservation from FDT {start_addr} (0x{:x} bytes)",
                res.size
            );

            alloc.add_reservation(PhysMemoryRegion::new(start_addr, res.size))?;

            Ok(())
        })?;

    // Reserve the kernel address.
    info!("Reserving kernel text {image_start} - {image_end}");
    alloc.add_reservation(PhysMemoryRegion::from_start_end_address(
        image_start,
        image_end,
    ))?;

    // Reserve the DTB.
    info!("Reserving FDT {dtb_ptr} (0x{:04x} bytes)", dt.total_size());
    alloc.add_reservation(PhysMemoryRegion::new(dtb_ptr.to_untyped(), dt.total_size()))?;

    // Reserve the initrd.
    if let Some(chosen) = dt.find_nodes("/chosen").next()
        && let Some(start_addr) = chosen
            .find_property("linux,initrd-start")
            .map(|prop| prop.u64())
        && let Some(end_addr) = chosen
            .find_property("linux,initrd-end")
            .map(|prop| prop.u64())
    {
        info!("Reserving initrd 0x{start_addr:X} - 0x{end_addr:X}");
        alloc.add_reservation(PhysMemoryRegion::from_start_end_address(
            PA::from_value(start_addr as _),
            PA::from_value(end_addr as _),
        ))?;
    }

    set_kimage_start(image_start);

    Ok(())
}

pub fn allocate_kstack_region() -> VirtMemoryRegion {
    static mut CURRENT_VA: VA = VA::from_value(0xffff_b800_0000_0000);

    let range = VirtMemoryRegion::new(unsafe { CURRENT_VA }, KERNEL_STACK_SZ);

    // Add a guard page between allocations.
    unsafe { CURRENT_VA = range.end_address().add_pages(1) };

    range
}

// Returns the address that should be loaded into the SP.
pub fn setup_stack_and_heap(pgtbl_base: TPA<PgTableArray<L0Table>>) -> Result<VA> {
    let mut alloc = INITAL_ALLOCATOR.lock_save_irq();
    let alloc = alloc.as_mut().unwrap();

    // allocate the stack.
    let stack = alloc.alloc(KERNEL_STACK_SZ, PAGE_SIZE)?;
    let stack_phys_region = PhysMemoryRegion::new(stack, KERNEL_STACK_SZ);
    let stack_virt_region = allocate_kstack_region();

    // allocate the heap.
    let heap = alloc.alloc(KERNEL_HEAP_SZ, PAGE_SIZE)?;
    let heap_va = VA::from_value(0xffff_b000_0000_0000);
    let heap_phys_region = PhysMemoryRegion::new(heap, KERNEL_HEAP_SZ);
    let heap_virt_region = VirtMemoryRegion::new(heap_va, KERNEL_HEAP_SZ);

    let mut pg_alloc = SmallocPageAlloc::new(alloc);
    let mut ctx = MappingContext {
        allocator: &mut pg_alloc,
        mapper: &mut PageOffsetPgTableMapper {},
        invalidator: &AllEl1TlbInvalidator::new(),
    };

    // Map the stack.
    map_range(
        pgtbl_base,
        MapAttributes {
            phys: stack_phys_region,
            virt: stack_virt_region,
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(false),
        },
        &mut ctx,
    )?;

    // Map the heap.
    map_range(
        pgtbl_base,
        MapAttributes {
            phys: heap_phys_region,
            virt: heap_virt_region,
            mem_type: MemoryType::Normal,
            perms: PtePermissions::rw(false),
        },
        &mut ctx,
    )?;

    unsafe {
        HEAP_ALLOCATOR.0.lock_save_irq().init(
            heap_virt_region.start_address().as_ptr_mut().cast(),
            heap_virt_region.size(),
        )
    };

    Ok(stack_virt_region.end_address())
}
