use crate::{
    arch::{ArchImpl, x86_64::exceptions::ExceptionState},
    memory::{PageOffsetTranslator, page::ClaimedPage},
    process::owned::OwnedTask,
};
use core::arch::global_asm;
use libkernel::{
    UserAddressSpace, VirtualMemory,
    memory::{
        address::VA,
        permissions::PtePermissions,
        proc_vm::vmarea::{VMAPermissions, VMArea, VMAreaKind},
        region::VirtMemoryRegion,
    },
};

global_asm!(include_str!("idle.s"));

pub fn create_idle_task() -> OwnedTask {
    // We allocate a page for the idle code.
    let code_page = ClaimedPage::alloc_zeroed().expect("Failed to allocate idle page").leak();
    // Arbitrary address for idle code.
    let code_addr = VA::from_value(0xd00d0000);

    unsafe extern "C" {
        static __idle_start: u8;
        static __idle_end: u8;
    }

    let idle_start_ptr = unsafe { &__idle_start } as *const u8;
    let idle_end_ptr = unsafe { &__idle_end } as *const u8;
    let code_sz = unsafe { idle_end_ptr.offset_from(idle_start_ptr) as usize };

    unsafe {
        idle_start_ptr.copy_to(
            code_page
                .pa()
                .to_va::<PageOffsetTranslator>()
                .cast::<u8>()
                .as_ptr_mut(),
            code_sz,
        )
    };

    let mut addr_space = <ArchImpl as VirtualMemory>::ProcessAddressSpace::new().unwrap();

    // Map the code page into the address space
    // Note: map_page is stubbed in libkernel currently, so this won't actually update page tables yet.
    addr_space
        .map_page(code_page, code_addr, PtePermissions::rx(true))
        .unwrap();

    let ctx = ExceptionState {
        regs: [0; 16],
        rip: code_addr.value() as u64,
        rsp: 0, // Idle task usually doesn't need a user stack if it just hlts
        rflags: 0x202, // Interrupts enabled (IF=1) + Reserved(1)
    };

    let code_map = VMArea::new(
        VirtMemoryRegion::new(code_addr, code_sz),
        VMAreaKind::Anon,
        VMAPermissions::rx(),
    );

    OwnedTask::create_idle_task(addr_space, ctx, code_map)
}
