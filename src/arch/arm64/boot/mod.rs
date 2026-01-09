use super::{
    exceptions::{ExceptionState, secondary_exceptions_init},
    memory::{fixmap::FIXMAPS, mmu::setup_kern_addr_space},
    proc::vdso::vdso_init,
};
use crate::drivers::timer::kick_current_cpu;
use crate::{
    arch::{ArchImpl, arm64::exceptions::exceptions_init},
    console::setup_console_logger,
    drivers::{
        fdt_prober::{probe_for_fdt_devices, set_fdt_va},
        init::run_initcalls,
    },
    interrupts::{cpu_messenger::cpu_messenger_init, get_interrupt_root},
    kmain,
    memory::{INITAL_ALLOCATOR, PAGE_ALLOC},
    sched::{sched_init_secondary, uspc_ret::dispatch_userspace_task},
};
use aarch64_cpu::{
    asm::{self, barrier},
    registers::{ReadWriteable, SCTLR_EL1, TCR_EL1, TTBR0_EL1},
};
use core::arch::global_asm;
use libkernel::{
    CpuOps,
    arch::arm64::memory::pg_tables::{L0Table, PgTableArray},
    error::Result,
    memory::{
        address::{PA, TPA, VA},
        page_alloc::FrameAllocator,
    },
    sync::per_cpu::setup_percpu,
};
use logical_map::setup_logical_map;
use memory::{setup_allocator, setup_stack_and_heap};
use secondary::{boot_secondaries, cpu_count, save_idmap, secondary_booted};

mod exception_level;
mod logical_map;
pub(super) mod memory;
mod paging_bootstrap;
pub(super) mod secondary;

global_asm!(include_str!("start.s"));

/// Stage 1 Initialize of the system architechture.
///
/// This function is called by the main primary CPU with the other CPUs parked.
/// All interrupts should be disabled, the ID map setup in TTBR0 and the highmem
/// map setup in TTBR1.
///
/// The memory map is setup as follows:
///
/// 0xffff_0000_0000_0000 - 0xffff_8000_0000_0000 | Logical Memory Map
/// 0xffff_8000_0000_0000 - 0xffff_8000_1fff_ffff | Kernel image
/// 0xffff_8100_0000_0000 - 0xffff_8100_0000_1000 | VDSO (userspace)
/// 0xffff_9000_0000_0000 - 0xffff_9000_0020_1fff | Fixed mappings
/// 0xffff_b000_0000_0000 - 0xffff_b000_0400_0000 | Kernel Heap
/// 0xffff_b800_0000_0000 - 0xffff_b800_0000_8000 | Kernel Stack (per CPU)
/// 0xffff_d000_0000_0000 - 0xffff_d000_ffff_ffff | MMIO remap
/// 0xffff_e000_0000_0000 - 0xffff_e000_0000_0800 | Exception Vector Table
///
/// Returns the stack pointer in X0, which should be hen set by the boot asm.
#[unsafe(no_mangle)]
fn arch_init_stage1(
    dtb_ptr: TPA<u8>,
    image_start: PA,
    image_end: PA,
    highmem_pgtable_base: TPA<PgTableArray<L0Table>>,
) -> VA {
    (|| -> Result<VA> {
        setup_console_logger();

        setup_allocator(dtb_ptr, image_start, image_end)?;

        let dtb_addr = {
            let mut fixmaps = FIXMAPS.lock_save_irq();
            fixmaps.setup_fixmaps(highmem_pgtable_base);

            unsafe { fixmaps.remap_fdt(dtb_ptr) }.unwrap()
        };

        set_fdt_va(dtb_addr.cast());
        setup_logical_map(highmem_pgtable_base)?;
        let stack_addr = setup_stack_and_heap(highmem_pgtable_base)?;
        setup_kern_addr_space(highmem_pgtable_base)?;

        Ok(stack_addr)
    })()
    .unwrap_or_else(|_| park_cpu())
}

#[unsafe(no_mangle)]
fn arch_init_stage2(frame: *mut ExceptionState) -> *mut ExceptionState {
    // Save the ID map addr for booting secondaries.
    save_idmap(PA::from_value(TTBR0_EL1.get_baddr() as _));

    // Disable the ID map.
    TCR_EL1.modify(TCR_EL1::EPD0::DisableTTBR0Walks);
    barrier::isb(barrier::SY);

    // We now have enough memory setup to switch to the real page allocator.
    let smalloc = INITAL_ALLOCATOR
        .lock_save_irq()
        .take()
        .expect("Smalloc should not have been taken yet");

    let page_alloc = unsafe { FrameAllocator::init(smalloc) };

    if PAGE_ALLOC.set(page_alloc).is_err() {
        panic!("Cannot setup physical memory allocator");
    }

    // Don't trap wfi/wfe in el0.
    SCTLR_EL1.modify(SCTLR_EL1::NTWE::DontTrap + SCTLR_EL1::NTWI::DontTrap);

    exceptions_init().expect("Failed to initialize exceptions");
    ArchImpl::enable_interrupts();

    unsafe { run_initcalls() };
    probe_for_fdt_devices();

    unsafe { setup_percpu(cpu_count()) };

    cpu_messenger_init(cpu_count());

    if let Err(e) = vdso_init() {
        panic!("VDSO setup failed: {e}");
    }

    let cmdline = super::fdt::get_cmdline();

    kmain(cmdline.unwrap_or_default(), frame);

    boot_secondaries();

    // Prove that we can send IPIs through the messenger.
    frame
}

fn arch_init_secondary(ctx_frame: *mut ExceptionState) -> *mut ExceptionState {
    // Disable the ID map.
    TCR_EL1.modify(TCR_EL1::EPD0::DisableTTBR0Walks);
    barrier::isb(barrier::SY);

    // Enable interrupts and exceptions.
    secondary_exceptions_init();

    if let Some(ic) = get_interrupt_root() {
        ic.enable_core(ArchImpl::id());
    }

    // Arm the per-CPU system timer so this core starts receiving timer IRQs.
    kick_current_cpu();

    ArchImpl::enable_interrupts();

    secondary_booted();

    sched_init_secondary();

    dispatch_userspace_task(ctx_frame);

    ctx_frame
}

#[unsafe(no_mangle)]
pub extern "C" fn park_cpu() -> ! {
    loop {
        asm::wfe();
    }
}
