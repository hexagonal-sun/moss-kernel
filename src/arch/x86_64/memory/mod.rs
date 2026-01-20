use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
};

use libkernel::memory::address::{PA, VA};
use linked_list_allocator::Heap;

use crate::sync::SpinLock;

pub const PAGE_OFFSET: usize = 0xffff_0000_0000_0000; // Unused for now but kept for compatibility

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
