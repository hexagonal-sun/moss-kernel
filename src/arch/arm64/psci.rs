use core::arch::naked_asm;
use libkernel::memory::address::PA;

pub struct PSCIEntry {
    pub method: PSCIMethod,
    pub cpu_on_id: Option<u32>,
}

pub enum PSCIMethod {
    Hvc,
    Smc,
}

const CPU_ON_ID: u32 = 0xc400_0003;

// Re-export the low-level PSCI helpers so other modules (e.g. `arch::arm64::mod`)
// can invoke them without repeating the `use` dance.

pub fn boot_secondary_psci(entry: PSCIEntry, core_id: usize, entry_fn: PA, ctx: PA) {
    let method_id = entry.cpu_on_id.unwrap_or(CPU_ON_ID);

    match entry.method {
        PSCIMethod::Hvc => unsafe {
            do_psci_hyp_call(
                method_id,
                core_id as _,
                entry_fn.value() as _,
                ctx.value() as _,
            )
        },
        PSCIMethod::Smc => unsafe {
            do_psci_smc_call(
                method_id,
                core_id as _,
                entry_fn.value() as _,
                ctx.value() as _,
            )
        },
    };
}

#[unsafe(naked)]
pub unsafe extern "C" fn do_psci_hyp_call(id: u32, arg1: u64, arg2: u64, arg3: u64) -> i64 {
    naked_asm!("hvc #0", "ret")
}

#[unsafe(naked)]
pub unsafe extern "C" fn do_psci_smc_call(id: u32, arg1: u64, arg2: u64, arg3: u64) -> i64 {
    naked_asm!("smc #0", "ret")
}
