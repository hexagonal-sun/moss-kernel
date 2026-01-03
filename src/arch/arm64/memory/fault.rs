use core::mem;

use crate::{
    arch::arm64::{
        boot::memory::KERNEL_STACK_AREA,
        exceptions::{
            ExceptionState,
            esr::{AbortIss, Exception, IfscCategory},
        },
        memory::uaccess::UAccessResult,
    },
    memory::fault::{FaultResolution, handle_demand_fault, handle_protection_fault},
    sched::{current::current_task, spawn_kernel_work},
};
use alloc::boxed::Box;
use libkernel::{
    UserAddressSpace,
    error::Result,
    memory::{address::VA, proc_vm::vmarea::AccessKind, region::VirtMemoryRegion},
};

#[repr(C)]
struct FixupTable {
    start: VA,
    end: VA,
    fixup: VA,
}

unsafe extern "C" {
    static __UACCESS_FIXUP: FixupTable;
}

impl FixupTable {
    fn is_in_fixup(&self, addr: VA) -> bool {
        VirtMemoryRegion::from_start_end_address(self.start, self.end).contains_address(addr)
    }
}

fn run_mem_fault_handler(exception: Exception, info: AbortIss) -> Result<FaultResolution> {
    let access_kind = determine_access_kind(exception, info);

    if let Some(far) = info.far {
        let fault_addr = VA::from_value(far as usize);

        let task = current_task();
        let mut vm = task.vm.lock_save_irq();

        match info.ifsc.category() {
            IfscCategory::TranslationFault => handle_demand_fault(&mut vm, fault_addr, access_kind),
            IfscCategory::PermissionFault => {
                let pg_info = vm
                    .mm_mut()
                    .address_space_mut()
                    .translate(fault_addr)
                    .expect("Could not find PTE in permission fault");

                handle_protection_fault(&mut vm, fault_addr, access_kind, pg_info)
            }
            _ => panic!("Unhandled memory fault"),
        }
    } else {
        panic!("Instruction/Data abort with no valid Fault Address Register",);
    }
}

fn handle_uacess_abort(exception: Exception, info: AbortIss, state: &mut ExceptionState) {
    match run_mem_fault_handler(exception, info) {
        // We mapped in a page, the uacess handler can proceed.
        Ok(FaultResolution::Resolved) => (),
        // If the fault coldn't be resolved, signal to the uacess fixup that
        // the abort failed.
        Ok(FaultResolution::Denied) => {
            state.x[0] = UAccessResult::AbortDenied as _;
            state.elr_el1 = unsafe { __UACCESS_FIXUP.fixup.value() as u64 };
        }
        // If the page fault involves sleepy kernel work, we send that work
        // over to the uacess future for it to then await it.
        Ok(FaultResolution::Deferred(fut)) => {
            let ptr = Box::into_raw(fut);

            // A fat pointer is guaranteed to be a (data_ptr, vtable_ptr)
            // pair. Transmute it into a tuple of thin pointers to get the
            // components.
            let (data_ptr, vtable_ptr): (*mut (), *const ()) = unsafe { mem::transmute(ptr) };

            state.x[0] = UAccessResult::AbortDeferred as _;
            state.x[1] = data_ptr as _;
            state.x[3] = vtable_ptr as _;
            state.elr_el1 = unsafe { __UACCESS_FIXUP.fixup.value() as u64 };
        }
        Err(_) => panic!("Page fault handler error, SIGBUS on process"),
    }
}

pub fn handle_kernel_mem_fault(exception: Exception, info: AbortIss, state: &mut ExceptionState) {
    if unsafe { __UACCESS_FIXUP.is_in_fixup(VA::from_value(state.elr_el1 as usize)) } {
        handle_uacess_abort(exception, info, state);
        return;
    }

    // If the source of the fault (ELR), wasn't in the uacess fixup section,
    // then any abort genereated by the kernel is a panic since we don't
    // demand-page any kernel memory.
    //
    // Try and differentiate between a stack overflow condition and other
    // faults.
    if let Some(far) = info.far
        && KERNEL_STACK_AREA.contains_address(VA::from_value(far as _))
    {
        panic!("Kernel stack overflow detected.  Context:\n{}", state);
    } else {
        panic!("Kernel memory fault detected.  Context:\n{}", state);
    }
}

pub fn handle_mem_fault(exception: Exception, info: AbortIss) {
    match run_mem_fault_handler(exception, info) {
        Ok(FaultResolution::Resolved) => {}
        // TODO: Implement proc signals.
        Ok(FaultResolution::Denied) => panic!(
            "SIGSEGV on process {} {:?} PC: {:x}",
            current_task().process.tgid,
            exception,
            current_task().ctx.user().elr_el1
        ),
        // If the page fault involves sleepy kernel work, we can
        // spawn that work on the process, since there is no other
        // kernel work happening.
        Ok(FaultResolution::Deferred(fut)) => spawn_kernel_work(async {
            if Box::into_pin(fut).await.is_err() {
                panic!("Page fault defered error, SIGBUS on process");
            }
        }),
        Err(_) => panic!("Page fault handler error, SIGBUS on process"),
    }
}

fn determine_access_kind(exception: Exception, info: AbortIss) -> AccessKind {
    if matches!(exception, Exception::InstrAbortLowerEL(_)) {
        AccessKind::Execute
    } else {
        // We know it must be a DataAbort, so we can use `info.write`.
        if info.write {
            AccessKind::Write
        } else {
            AccessKind::Read
        }
    }
}
