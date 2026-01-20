use alloc::sync::Arc;
use core::arch::asm;
use core::future::Future;

use crate::{
    process::{Task, owned::OwnedTask, thread_group::signal::{SigId, ksigaction::UserspaceSigAction}},
    sync::SpinLock,
};
use alloc::boxed::Box;
use libkernel::{CpuOps, VirtualMemory, UserAddressSpace, error::Result, memory::address::{UA, VA}};
use crate::memory::uaccess::UserCopyable;

use self::exceptions::ExceptionState;
use crate::memory::PageOffsetTranslator;
use libkernel::error::KernelError;
use log::{info, warn};
use crate::per_cpu;
use libkernel::sync::once_lock::OnceLock;

static MULTIBOOT_INFO_ADDR: OnceLock<usize, X86_64> = OnceLock::new();
static KERNEL_ADDRESS_SPACE: OnceLock<SpinLock<libkernel::arch::x86_64::memory::X86_64KernelAddressSpace>, X86_64> = OnceLock::new();

mod ptrace;
pub use ptrace::X86_64PtraceGPRegs;

// Boot assembly
core::arch::global_asm!(include_str!("boot/header.s"));
core::arch::global_asm!(include_str!("boot/start.s"));

// MSRs for Syscall
const MSR_STAR: u32 = 0xC0000081;
const MSR_LSTAR: u32 = 0xC0000082;
const MSR_FMASK: u32 = 0xC0000084;
const MSR_KERNEL_GS_BASE: u32 = 0xC0000102;
const MSR_EFER: u32 = 0xC0000080;

pub fn enable_syscalls() {
    unsafe {
        // Enable SCE (Syscall Extensions) in EFER
        let mut efer = rdmsr(MSR_EFER);
        efer |= 1; // Bit 0 is SCE
        wrmsr(MSR_EFER, efer);

        // Set LSTAR to syscall_entry
        unsafe extern "C" {
            fn syscall_entry();
        }
        wrmsr(MSR_LSTAR, syscall_entry as *const () as usize as u64);

        // Set STAR: User CS/SS at 48..63, Kernel CS/SS at 32..47
        // Kernel CS is usually 0x8 (selector 1), SS 0x10 (selector 2)
        // User CS 0x18 (selector 3), SS 0x20 (selector 4) + 3 (for RPL 3) = 0x1b, 0x23
        // SYSRET loads CS from STAR[48:63] + 16, SS from STAR[48:63] + 8
        // SYSCALL loads CS from STAR[32:47], SS from STAR[32:47] + 8
        // Let's assume standard kernel layout for now: Kernel code 0x8, data 0x10. User code 0x18, data 0x20.
        // We need to verify GDT layout eventually.
        let kernel_star = (0x08u64) << 32;
        let user_star = (0x10u64) << 48; // SYSRET uses this base to calculate CS/SS
        wrmsr(MSR_STAR, kernel_star | user_star);

        // Set FMASK: Mask interrupts (IF=0x200) and Direction (DF=0x400)
        wrmsr(MSR_FMASK, 0x200);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn x86_64_start(multiboot_info_addr: usize) -> ! {
    MULTIBOOT_INFO_ADDR.set(multiboot_info_addr).ok();
    // RAW DEBUG: 'R' (Rust start)
    unsafe {
        asm!("mov dx, 0x3f8", "mov al, 0x52", "out dx, al", options(nomem, nostack, preserves_flags));
    }
    // Initialize memory allocator manually for QEMU test
    use crate::memory::INITAL_ALLOCATOR;
    use libkernel::memory::region::PhysMemoryRegion;
    use libkernel::memory::address::PA;

    unsafe {
         // Initialize memory allocator manually for QEMU test
         let mut alloc = INITAL_ALLOCATOR.lock_save_irq();
         if let Some(ref mut smalloc) = *alloc {
             // Add 128MB of memory at 256MB mark (0x10000000 -> 0x18000000)
             // This assumes we have at least 512MB RAM
             smalloc.add_memory(PhysMemoryRegion::new(PA::from_value(0x10000000), 0x8000000)).expect("Failed to add memory");
         }
         
         // Initialize Heap
         // Use 16MB at 240MB mark (0xF000000)
         crate::arch::x86_64::memory::HEAP_ALLOCATOR.0.lock_save_irq().init(0xF000000 as *mut u8, 0x1000000);
          // Initialize PAGE_ALLOC from INITAL_ALLOCATOR
          let smalloc = alloc.take().expect("INITAL_ALLOCATOR empty");
          drop(alloc);
          let page_alloc = unsafe { libkernel::memory::page_alloc::FrameAllocator::init(smalloc) };
          if crate::memory::PAGE_ALLOC.set(page_alloc).is_err() {
              panic!("PAGE_ALLOC already set");
          }

          // Initialize PerCpu
          unsafe { libkernel::sync::per_cpu::setup_percpu(1); }

          // Initialize Kernel Address Space
          KERNEL_ADDRESS_SPACE.set(SpinLock::new(libkernel::arch::x86_64::memory::X86_64KernelAddressSpace {})).ok();

          // Initialize Timer
          let tsc_timer = crate::drivers::timer::x86_tsc::X86TscTimer::new();
          crate::drivers::timer::register_timer(Arc::new(tsc_timer));

          // Initialize early console and logger
          crate::drivers::uart::x86_uart::early_x86_uart_init();
          crate::console::setup_console_logger();

          // Enable syscalls
          enable_syscalls();

          // RAW DEBUG: 'C' (Console ready)
          unsafe {
              asm!("mov dx, 0x3f8", "mov al, 0x43", "out dx, al", options(nomem, nostack, preserves_flags));
          }
    }

    // Call kmain with init args
    // --init=/bin/usertest --rootfs=ext4fs simulates command line args
    let args = alloc::string::String::from("--init /bin/usertest --rootfs ext4fs --automount /dev,devfs"); // kmain parses args

    // RAW DEBUG: 'K' (Kmain)
    unsafe {
        asm!("mov dx, 0x3f8", "mov al, 0x4b", "out dx, al", options(nomem, nostack, preserves_flags));
    }

    crate::kmain(args, core::ptr::null_mut());

    loop {
        unsafe { asm!("hlt"); }
    }
}

unsafe fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    unsafe { asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high); }
    ((high as u64) << 32) | (low as u64)
}

unsafe fn wrmsr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    unsafe { asm!("wrmsr", in("ecx") msr, in("eax") low, in("edx") high); }
}

mod exceptions;
mod proc;

pub mod boot;


pub struct X86_64 {}

impl CpuOps for X86_64 {
    fn id() -> usize { 0 }

    fn halt() -> ! {
        loop {
            unsafe { asm!("hlt"); }
        }
    }

    fn disable_interrupts() -> usize { 0 }

    fn restore_interrupt_state(_flags: usize) {}

    fn enable_interrupts() {}
}

impl VirtualMemory for X86_64 {
    type PageTableRoot = ();
    type ProcessAddressSpace = libkernel::arch::x86_64::memory::X86_64ProcessAddressSpace;
    type KernelAddressSpace = libkernel::arch::x86_64::memory::X86_64KernelAddressSpace;

    const PAGE_OFFSET: usize = 0;

    fn kern_address_space() -> &'static SpinLock<Self::KernelAddressSpace> {
        KERNEL_ADDRESS_SPACE.get().expect("x86_64 kernel address space not initialized")
    }
}

pub mod memory;


impl crate::arch::Arch for X86_64 {
    type UserContext = exceptions::ExceptionState;
    type PTraceGpRegs = X86_64PtraceGPRegs;

    fn new_user_context(_entry_point: VA, _stack_top: VA) -> Self::UserContext {
        exceptions::ExceptionState::default()
    }

    fn name() -> &'static str { "x86_64" }

    fn cpu_count() -> usize { 1 }

    fn do_signal(
        _sig: SigId,
        _action: UserspaceSigAction,
    ) -> impl Future<Output = Result<<Self as crate::arch::Arch>::UserContext>> {
        async { Err(libkernel::error::KernelError::NotSupported) }
    }

    fn do_signal_return() -> impl Future<Output = Result<<Self as crate::arch::Arch>::UserContext>> {
        async { Err(libkernel::error::KernelError::NotSupported) }
    }

    fn context_switch(new: Arc<Task>) {
        proc::context_switch(new);
    }

    fn create_idle_task() -> OwnedTask {
        proc::idle::create_idle_task()
    }

    fn power_off() -> ! { loop { unsafe { asm!("hlt"); } } }

    fn restart() -> ! { loop { unsafe { asm!("hlt"); } } }

    unsafe fn copy_from_user(
        src: UA,
        dst: *mut (),
        len: usize,
    ) -> impl Future<Output = Result<()>> {
        use crate::memory::PageOffsetTranslator;
        use libkernel::memory::PAGE_SIZE;

        use alloc::sync::Arc;
        use libkernel::memory::proc_vm::vmarea::AccessKind;

        let dst_addr = dst as usize;
        async move {
            let mut remaining = len;
            let mut cur = src.value();
            let mut dst_addr = dst_addr;

            while remaining > 0 {
                let page_offset = cur & (PAGE_SIZE - 1);
                let to_copy = core::cmp::min(remaining, PAGE_SIZE - page_offset);

                let task: Arc<crate::process::Task> = crate::sched::current::current_task_shared();
                let page_alloc = unsafe { task.get_page(UA::from_value(cur), AccessKind::Read).await? };

                let pa = page_alloc.region().start_address();
                let va = pa.to_va::<PageOffsetTranslator>();
                let src_ptr = unsafe { (va.as_ptr() as *const u8).add(page_offset) };
                let dstp = dst_addr as *mut u8;

                unsafe { core::ptr::copy_nonoverlapping(src_ptr, dstp, to_copy) };

                drop(page_alloc);

                remaining -= to_copy;
                cur += to_copy;
                dst_addr += to_copy;
            }

            Ok(())
        }
    }

    unsafe fn try_copy_from_user(src: UA, dst: *mut (), len: usize) -> Result<()> {
        use libkernel::memory::PAGE_SIZE;


        let mut remaining = len;
        let mut cur = src.value();
        let mut dstp = dst as *mut u8;

        let task = crate::sched::current::current_task_shared();
        let mut vm = task.vm.lock_save_irq();

        while remaining > 0 {
            let page_offset = cur & (PAGE_SIZE - 1);
            let to_copy = core::cmp::min(remaining, PAGE_SIZE - page_offset);

            let va = libkernel::memory::address::VA::from_value(cur);
            if let Some(pi) = vm.mm_mut().address_space_mut().translate(va) {
                if !pi.perms.is_read() {
                    return Err(KernelError::Fault);
                }

                let pa = pi.pfn.as_phys_range().start_address();
                let kva = pa.to_va::<PageOffsetTranslator>();
                let src_ptr = unsafe { (kva.as_ptr() as *const u8).add(page_offset) };

                unsafe { core::ptr::copy_nonoverlapping(src_ptr, dstp, to_copy) };
            } else {
                return Err(KernelError::Fault);
            }

            remaining -= to_copy;
            cur += to_copy;
            dstp = unsafe { dstp.add(to_copy) };
        }

        Ok(())
    }

    unsafe fn copy_to_user(
        src: *const (),
        dst: UA,
        len: usize,
    ) -> impl Future<Output = Result<()>> {
        use crate::memory::PageOffsetTranslator;
        use libkernel::memory::PAGE_SIZE;
        use libkernel::memory::proc_vm::vmarea::AccessKind;
        use alloc::sync::Arc;

        let src_addr = src as usize;
        async move {
            let mut remaining = len;
            let mut cur = dst.value();
            let mut src_addr = src_addr;

            while remaining > 0 {
                let page_offset = cur & (PAGE_SIZE - 1);
                let to_copy = core::cmp::min(remaining, PAGE_SIZE - page_offset);

                let task: Arc<crate::process::Task> = crate::sched::current::current_task_shared();
                let page_alloc = unsafe { task.get_page(UA::from_value(cur), AccessKind::Write).await? };

                let pa = page_alloc.region().start_address();
                let kva = pa.to_va::<PageOffsetTranslator>();
                let dst_ptr = unsafe { (kva.as_ptr_mut() as *mut u8).add(page_offset) };
                let srcp = src_addr as *const u8;

                unsafe { core::ptr::copy_nonoverlapping(srcp, dst_ptr, to_copy) };

                drop(page_alloc);

                remaining -= to_copy;
                cur += to_copy;
                src_addr += to_copy;
            }

            Ok(())
        }
    }

    unsafe fn copy_strn_from_user(
        src: UA,
        dst: *mut u8,
        len: usize,
    ) -> impl Future<Output = Result<usize>> {
        use crate::memory::PageOffsetTranslator;
        use libkernel::memory::PAGE_SIZE;
        use libkernel::memory::proc_vm::vmarea::AccessKind;
        use alloc::sync::Arc;

        let dst_addr = dst as usize;
        async move {
            let mut remaining = len;
            let mut cur = src.value();
            let mut dst_addr = dst_addr;
            let mut total_copied = 0usize;

            while remaining > 0 {
                let page_offset = cur & (PAGE_SIZE - 1);
                let to_copy = core::cmp::min(remaining, PAGE_SIZE - page_offset);

                let task: Arc<crate::process::Task> = crate::sched::current::current_task_shared();
                let page_alloc = unsafe { task.get_page(UA::from_value(cur), libkernel::memory::proc_vm::vmarea::AccessKind::Read).await? };

                let pa = page_alloc.region().start_address();
                let kva = pa.to_va::<PageOffsetTranslator>();
                let src_ptr = unsafe { (kva.as_ptr() as *const u8).add(page_offset) };
                let dstp = dst_addr as *mut u8;

                let slice = unsafe { core::slice::from_raw_parts(src_ptr, to_copy) };
                if let Some(pos) = slice.iter().position(|&b| b == 0) {
                    unsafe { core::ptr::copy_nonoverlapping(src_ptr, dstp, pos + 1) };
                    total_copied += pos + 1;
                    return Ok(total_copied);
                } else {
                    unsafe { core::ptr::copy_nonoverlapping(src_ptr, dstp, to_copy) };
                    total_copied += to_copy;
                }

                remaining -= to_copy;
                cur += to_copy;
                dst_addr += to_copy;
            }

            Ok(total_copied)
        }
    }
}

impl libkernel::memory::page_alloc::PageAllocGetter<X86_64> for X86_64 {
    fn global_page_alloc() -> &'static libkernel::sync::once_lock::OnceLock<libkernel::memory::page_alloc::FrameAllocator<X86_64>, X86_64> {
        &crate::memory::PAGE_ALLOC
    }
}

pub fn get_initrd_region() -> Option<libkernel::memory::region::PhysMemoryRegion> {
    let addr = *MULTIBOOT_INFO_ADDR.get()?;
    let flags = unsafe { *(addr as *const u32) };
    
    info!("Multiboot flags: 0x{:x}", flags);
    
    // Check if bit 3 (mods) is set
    if (flags & (1 << 3)) == 0 {
        info!("Multiboot mods bit not set in flags");
        return None;
    }
    
    let mods_count = unsafe { *( (addr + 20) as *const u32 ) };
    let mods_addr = unsafe { *( (addr + 24) as *const u32 ) };
    
    info!("Multiboot mods: count={}, addr=0x{:x}", mods_count, mods_addr);
    
    if mods_count == 0 {
        return None;
    }
    
    let mod_start = unsafe { *( mods_addr as *const u32 ) };
    let mod_end = unsafe { *( (mods_addr + 4) as *const u32 ) };
    
    info!("Multiboot mod[0] region: 0x{:x} - 0x{:x}", mod_start, mod_end);
    
    use libkernel::memory::address::PA;
    use libkernel::memory::region::PhysMemoryRegion;
    
    Some(PhysMemoryRegion::from_start_end_address(
        PA::from_value(mod_start as usize),
        PA::from_value(mod_end as usize)
    ))
}
