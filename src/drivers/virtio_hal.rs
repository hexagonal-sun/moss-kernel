use crate::arch::ArchImpl;
use crate::memory::PageOffsetTranslator;
use core::ptr::NonNull;
use libkernel::VirtualMemory;
use libkernel::memory::PAGE_SIZE;
use libkernel::memory::address::{PA, TPA};
use libkernel::memory::region::PhysMemoryRegion;
use log::trace;
use virtio_drivers::{BufferDirection, Hal, PhysAddr};

pub(super) struct VirtioHal;

impl VirtioHal {
    #[inline]
    fn pages_to_order(pages: usize) -> u8 {
        let pages = pages.max(1);
        let rounded = pages.next_power_of_two();
        rounded.ilog2() as u8
    }
}

unsafe impl Hal for VirtioHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let order = Self::pages_to_order(pages);

        let region = crate::memory::PAGE_ALLOC
            .get()
            .expect("PAGE_ALLOC not initialized")
            .alloc_frames(order)
            .expect("virtio dma_alloc: out of memory")
            .leak();

        let region_start = region.start_address();
        let paddr = region_start.value() as PhysAddr;

        // Convert PA->VA using the kernel's direct mapping window.
        let vaddr = region_start.to_va::<PageOffsetTranslator>().as_ptr_mut() as *mut u8;
        let vaddr = NonNull::new(vaddr).expect("virtio dma_alloc: null vaddr");

        unsafe {
            core::ptr::write_bytes(vaddr.as_ptr(), 0, pages * PAGE_SIZE);
        }

        trace!("alloc DMA: paddr={paddr:#x}, pages={pages}, order={order}");
        (paddr, vaddr)
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        trace!("dealloc DMA: paddr={paddr:#x}, pages={pages}");

        let order = Self::pages_to_order(pages);
        let region = PhysMemoryRegion::new(
            PA::from_value(paddr as usize),
            (1usize << order) * PAGE_SIZE,
        );

        // SAFETY: `dma_alloc` leaked an allocation for exactly this region.
        let alloc = unsafe {
            crate::memory::PAGE_ALLOC
                .get()
                .expect("PAGE_ALLOC not initialized")
                .alloc_from_region(region)
        };

        drop(alloc);
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        let vaddr = TPA::from_value(paddr as usize)
            .to_va::<PageOffsetTranslator>()
            .as_ptr_mut();
        NonNull::new(vaddr).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        // We're assuming that all ram is DMA-coherent in QEMU.
        // We don't need to adjust page table mapping properties to disable caching and the like to make this work.
        let vaddr = buffer.as_ptr() as *mut u8 as usize;

        // Buffer must be in the direct map for this fast translation.
        if vaddr < ArchImpl::PAGE_OFFSET {
            panic!("virtio share: buffer VA is not in direct map: {:#x}", vaddr);
        }

        (vaddr - ArchImpl::PAGE_OFFSET) as PhysAddr
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {}
}
