use crate::arch::ArchImpl;
use crate::memory::PageOffsetTranslator;
use crate::sync::SpinLock;
use alloc::vec::Vec;
use core::ptr::NonNull;
use libkernel::memory::PAGE_SIZE;
use libkernel::memory::address::{PA, TPA, VA};
use libkernel::memory::proc_vm::address_space::VirtualMemory;
use libkernel::memory::region::PhysMemoryRegion;
use virtio_drivers::{BufferDirection, Hal, PhysAddr};

struct BouncedShare {
    paddr: PhysAddr,
    pages: usize,
}

static BOUNCED_SHARES: SpinLock<Vec<BouncedShare>> = SpinLock::new(Vec::new());

pub(super) struct VirtioHal;

impl VirtioHal {
    #[inline]
    fn pages_to_order(pages: usize) -> u8 {
        let pages = pages.max(1);
        let rounded = pages.next_power_of_two();
        rounded.ilog2() as u8
    }

    fn translated_phys_addr(vaddr: VA) -> Option<PhysAddr> {
        ArchImpl::kern_address_space()
            .lock_save_irq()
            .translate(vaddr)
            .map(|pa| pa.value() as PhysAddr)
    }

    fn translate_buffer(vaddr: VA, len: usize) -> Option<PhysAddr> {
        debug_assert!(len > 0);

        let first_page_va = vaddr.page_aligned();
        let last_byte_va = vaddr.add_bytes(len - 1);
        let last_page_va = last_byte_va.page_aligned();

        let first_page_pa = Self::translated_phys_addr(first_page_va)?;
        let mut page_va = first_page_va;
        let mut expected_page_pa = first_page_pa;

        loop {
            let page_pa = Self::translated_phys_addr(page_va)?;
            if page_pa != expected_page_pa {
                return None;
            }

            if page_va == last_page_va {
                break;
            }

            page_va = page_va.add_pages(1);
            expected_page_pa += PAGE_SIZE as PhysAddr;
        }

        Some(first_page_pa + vaddr.page_offset() as PhysAddr)
    }

    fn bounce_copy_in(paddr: PhysAddr, src: &[u8]) {
        let bounce = PA::from_value(paddr as usize)
            .cast::<u8>()
            .to_va::<PageOffsetTranslator>()
            .as_ptr_mut();

        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), bounce, src.len());
        }
    }

    fn bounce_copy_out(paddr: PhysAddr, dst: &mut [u8]) {
        let bounce = PA::from_value(paddr as usize)
            .cast::<u8>()
            .to_va::<PageOffsetTranslator>()
            .as_ptr();

        unsafe {
            core::ptr::copy_nonoverlapping(bounce, dst.as_mut_ptr(), dst.len());
        }
    }

    fn share_via_bounce(buffer: &[u8], direction: BufferDirection) -> PhysAddr {
        let pages = buffer.len().div_ceil(PAGE_SIZE);
        let (paddr, _vaddr) = <Self as Hal>::dma_alloc(pages, direction);

        if matches!(
            direction,
            BufferDirection::DriverToDevice | BufferDirection::Both
        ) {
            Self::bounce_copy_in(paddr, buffer);
        }

        BOUNCED_SHARES
            .lock_save_irq()
            .push(BouncedShare { paddr, pages });

        // trace!(
        //     "virtio share: bounced {:p} len={} direction={direction:?} to paddr={paddr:#x}",
        //     buffer.as_ptr(),
        //     buffer.len(),
        // );

        paddr
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

        // trace!("alloc DMA: paddr={paddr:#x}, pages={pages}, order={order}");
        (paddr, vaddr)
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        // trace!("dealloc DMA: paddr={paddr:#x}, pages={pages}");

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

    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> PhysAddr {
        // We're assuming that all RAM is DMA-coherent in QEMU, so once we have
        // a valid DMA-visible physical address we don't need extra cache
        // maintenance here.
        let buffer = unsafe { buffer.as_ref() };
        assert!(!buffer.is_empty(), "virtio share: empty buffer");
        let vaddr = VA::from_value(buffer.as_ptr() as usize);

        if let Some(paddr) = Self::translate_buffer(vaddr, buffer.len()) {
            return paddr;
        }

        Self::share_via_bounce(buffer, direction)
    }

    unsafe fn unshare(paddr: PhysAddr, mut buffer: NonNull<[u8]>, direction: BufferDirection) {
        let mut bounced = BOUNCED_SHARES.lock_save_irq();
        let Some(index) = bounced.iter().position(|share| share.paddr == paddr) else {
            return;
        };
        let pages = bounced.swap_remove(index).pages;
        drop(bounced);

        let buffer = unsafe { buffer.as_mut() };
        if matches!(
            direction,
            BufferDirection::DeviceToDriver | BufferDirection::Both
        ) {
            Self::bounce_copy_out(paddr, buffer);
        }

        let vaddr = PA::from_value(paddr as usize)
            .cast::<u8>()
            .to_va::<PageOffsetTranslator>()
            .as_ptr_mut();
        let vaddr = NonNull::new(vaddr).expect("virtio bounce buffer VA should never be null");

        unsafe {
            <Self as Hal>::dma_dealloc(paddr, vaddr, pages);
        }
    }
}
