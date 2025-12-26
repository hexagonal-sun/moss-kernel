use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
};

use libkernel::memory::address::{PA, VA};
use linked_list_allocator::Heap;

use crate::sync::SpinLock;

pub mod address_space;
pub mod fault;
pub mod fixmap;
pub mod mmu;
pub mod tlb;
pub mod uaccess;

pub const PAGE_OFFSET: usize = 0xffff_0000_0000_0000;
pub const IMAGE_BASE: VA = VA::from_value(0xffff_8000_0000_0000);
pub const FIXMAP_BASE: VA = VA::from_value(0xffff_9000_0000_0000);
pub const MMIO_BASE: VA = VA::from_value(0xffff_d000_0000_0000);
pub const EXCEPTION_BASE: VA = VA::from_value(0xffff_e000_0000_0000);

const BOGUS_START: PA = PA::from_value(usize::MAX);
static mut KIMAGE_START: PA = BOGUS_START;

pub struct SpinlockHeap(pub SpinLock<Heap>);

#[global_allocator]
pub static HEAP_ALLOCATOR: SpinlockHeap = SpinlockHeap(SpinLock::new(Heap::empty()));

unsafe impl GlobalAlloc for SpinlockHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0
            .lock_save_irq()
            .allocate_first_fit(layout)
            .ok()
            .map_or(core::ptr::null_mut(), |allocation| allocation.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            self.0
                .lock_save_irq()
                .deallocate(NonNull::new_unchecked(ptr), layout)
        }
    }
}

#[macro_export]
macro_rules! ksym_pa {
    ($sym:expr) => {{
        let v = libkernel::memory::address::VA::from_value(core::ptr::addr_of!($sym) as usize);
        $crate::arch::arm64::memory::translate_kernel_va(v)
    }};
}

#[macro_export]
macro_rules! kfunc_pa {
    ($sym:expr) => {{
        let v = libkernel::memory::address::VA::from_value($sym as usize);
        $crate::arch::arm64::memory::translate_kernel_va(v)
    }};
}

pub fn set_kimage_start(pa: PA) {
    unsafe {
        if KIMAGE_START != BOGUS_START {
            panic!("Attempted to change RAM_START, once set");
        }

        KIMAGE_START = pa;
    }
}

pub fn get_kimage_start() -> PA {
    unsafe {
        if KIMAGE_START == BOGUS_START {
            panic!("attempted to access RAM_START before being set");
        }

        KIMAGE_START
    }
}

pub fn translate_kernel_va(addr: VA) -> PA {
    let mut v = addr.value();

    v -= IMAGE_BASE.value();

    PA::from_value(v + get_kimage_start().value())
}
