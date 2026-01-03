use super::memory::{
    EXCEPTION_BASE,
    fault::{handle_kernel_mem_fault, handle_mem_fault},
};
use crate::{
    arch::{ArchImpl, arm64::boot::memory::KERNEL_STACK_PG_ORDER},
    interrupts::get_interrupt_root,
    ksym_pa,
    memory::PAGE_ALLOC,
    sched::{current::current_task, uspc_ret::dispatch_userspace_task},
    spawn_kernel_work,
};
use aarch64_cpu::registers::{CPACR_EL1, ReadWriteable, VBAR_EL1};
use core::{arch::global_asm, fmt::Display};
use esr::{Esr, Exception};
use libkernel::{
    KernAddressSpace, VirtualMemory,
    error::Result,
    memory::{
        address::VA,
        permissions::PtePermissions,
        region::{PhysMemoryRegion, VirtMemoryRegion},
    },
};
use syscall::handle_syscall;
use tock_registers::interfaces::Writeable;

pub mod esr;
mod syscall;

unsafe extern "C" {
    pub static __vectors_start: u8;
    pub static __vectors_end: u8;
}

#[unsafe(no_mangle)]
pub static EMERG_STACK_END: VA = VA::from_value(0xffff_c000_0000_0000);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExceptionState {
    pub x: [u64; 31],  // x0-x30
    pub elr_el1: u64,  // Exception link register
    pub spsr_el1: u64, // Saved program status register
    pub sp_el0: u64,   // Stack pointer of EL0
    pub tpid_el0: u64, // Thread process ID
}

impl Display for ExceptionState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "X0:  0x{:016x}, X1:  0x{:016x}", self.x[0], self.x[1])?;
        writeln!(f, "X2:  0x{:016x}, X3:  0x{:016x}", self.x[2], self.x[3])?;
        writeln!(f, "X4:  0x{:016x}, X5:  0x{:016x}", self.x[4], self.x[5])?;
        writeln!(f, "X6:  0x{:016x}, X7:  0x{:016x}", self.x[6], self.x[7])?;
        writeln!(f, "X8:  0x{:016x}, X9:  0x{:016x}", self.x[8], self.x[9])?;
        writeln!(f, "X10: 0x{:016x}, X11: 0x{:016x}", self.x[10], self.x[11])?;
        writeln!(f, "X12: 0x{:016x}, X13: 0x{:016x}", self.x[12], self.x[13])?;
        writeln!(f, "X14: 0x{:016x}, X15: 0x{:016x}", self.x[14], self.x[15])?;
        writeln!(f, "X16: 0x{:016x}, X17: 0x{:016x}", self.x[16], self.x[17])?;
        writeln!(f, "X18: 0x{:016x}, X19: 0x{:016x}", self.x[18], self.x[19])?;
        writeln!(f, "X20: 0x{:016x}, X21: 0x{:016x}", self.x[20], self.x[21])?;
        writeln!(f, "X22: 0x{:016x}, X23: 0x{:016x}", self.x[22], self.x[23])?;
        writeln!(f, "X24: 0x{:016x}, X25: 0x{:016x}", self.x[24], self.x[25])?;
        writeln!(f, "X26: 0x{:016x}, X27: 0x{:016x}", self.x[26], self.x[27])?;
        writeln!(f, "X28: 0x{:016x}, X29: 0x{:016x}", self.x[28], self.x[29])?;
        writeln!(f, "X30: 0x{:016x}", self.x[30])?;
        writeln!(f)?;
        writeln!(
            f,
            "ELR_EL1: 0x{:016x}, SPSR_EL1: 0x{:016x}",
            self.elr_el1, self.spsr_el1
        )?;
        writeln!(f, "ESR_EL1: {:x?}", Esr::read_el1())?;
        writeln!(
            f,
            "SP_EL0: 0x{:016x}, TPIDR_EL0: 0x{:016x}",
            self.sp_el0, self.tpid_el0
        )
    }
}

global_asm!(include_str!("exceptions.s"));

pub fn default_handler(state: &ExceptionState) {
    panic!("Unhandled CPU exception.  Program state:\n{}", state);
}

#[unsafe(no_mangle)]
extern "C" fn el1_sync_sp0(state: &mut ExceptionState) {
    default_handler(state);
}

#[unsafe(no_mangle)]
extern "C" fn el1_irq_sp0(state: &mut ExceptionState) {
    default_handler(state);
}

#[unsafe(no_mangle)]
extern "C" fn el1_fiq_sp0(state: &mut ExceptionState) {
    default_handler(state);
}

#[unsafe(no_mangle)]
extern "C" fn el1_serror_sp0(state: &mut ExceptionState) {
    default_handler(state);
}

#[unsafe(no_mangle)]
extern "C" fn el1_sync_spx(state: *mut ExceptionState) -> *const ExceptionState {
    let state = unsafe { state.as_mut().unwrap() };

    let esr = Esr::read_el1();

    let exception = esr.decode();

    match exception {
        Exception::InstrAbortCurrentEL(info) | Exception::DataAbortCurrentEL(info) => {
            handle_kernel_mem_fault(exception, info, state);
        }
        _ => default_handler(state),
    }

    state as *const _
}

#[unsafe(no_mangle)]
extern "C" fn el1_irq_spx(state: *mut ExceptionState) -> *const ExceptionState {
    match get_interrupt_root() {
        Some(ref im) => im.handle_interrupt(),
        None => panic!(
            "IRQ handled before root interrupt controller set.\n{}",
            unsafe { state.as_ref().unwrap() }
        ),
    }

    state
}

#[unsafe(no_mangle)]
extern "C" fn el1_fiq_spx(state: &mut ExceptionState) {
    default_handler(state);
}

#[unsafe(no_mangle)]
extern "C" fn el1_serror_spx(state: &mut ExceptionState) {
    default_handler(state);
}

#[unsafe(no_mangle)]
extern "C" fn el0_sync(state_ptr: *mut ExceptionState) -> *const ExceptionState {
    current_task().ctx.save_user_ctx(state_ptr);

    let state = unsafe { state_ptr.as_ref().unwrap() };

    let esr = Esr::read_el1();

    let exception = esr.decode();

    match exception {
        Exception::InstrAbortLowerEL(info) | Exception::DataAbortLowerEL(info) => {
            handle_mem_fault(exception, info);
        }
        Exception::SVC64(_) => {
            spawn_kernel_work(handle_syscall());
        }
        Exception::TrappedFP(_) => {
            CPACR_EL1.modify(CPACR_EL1::FPEN::TrapNothing);
            // TODO: Flag to start saving FP/SIMD context for this task and,
            // save the state.
        }
        _ => default_handler(state),
    }

    dispatch_userspace_task(state_ptr);

    state_ptr
}

#[unsafe(no_mangle)]
extern "C" fn el0_irq(state: *mut ExceptionState) -> *mut ExceptionState {
    current_task().ctx.save_user_ctx(state);

    match get_interrupt_root() {
        Some(ref im) => im.handle_interrupt(),
        None => panic!(
            "IRQ handled before root interrupt controller set.\n{}",
            unsafe { state.as_ref() }.unwrap()
        ),
    }

    dispatch_userspace_task(state);

    state
}

#[unsafe(no_mangle)]
extern "C" fn el0_fiq(state: &mut ExceptionState) {
    default_handler(state);
}

#[unsafe(no_mangle)]
extern "C" fn el0_serror(state: &mut ExceptionState) {
    default_handler(state);
}

pub fn exceptions_init() -> Result<()> {
    let start = ksym_pa!(__vectors_start);
    let end = ksym_pa!(__vectors_end);
    let region = PhysMemoryRegion::from_start_end_address(start, end);

    let mappable_region = region.to_mappable_region();

    let mut kspc = ArchImpl::kern_address_space().lock_save_irq();

    kspc.map_normal(
        mappable_region.region(),
        VirtMemoryRegion::new(EXCEPTION_BASE, mappable_region.region().size()),
        PtePermissions::rx(false),
    )?;

    let emerg_stack = PAGE_ALLOC
        .get()
        .unwrap()
        .alloc_frames(KERNEL_STACK_PG_ORDER as _)?
        .leak();

    kspc.map_normal(
        emerg_stack,
        VirtMemoryRegion::new(
            EMERG_STACK_END.sub_bytes(emerg_stack.size()),
            emerg_stack.size(),
        ),
        PtePermissions::rw(false),
    )?;

    secondary_exceptions_init();

    Ok(())
}

pub fn secondary_exceptions_init() {
    VBAR_EL1.set(EXCEPTION_BASE.value() as u64);
}
