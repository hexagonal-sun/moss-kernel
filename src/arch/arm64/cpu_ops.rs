use core::arch::asm;

/// Returns the current state of the interrupt flags (DAIF register) and disables IRQs.
#[inline(always)]
pub fn local_irq_save() -> u64 {
    let flags: u64;
    unsafe {
        asm!(
            "mrs {0}, daif",     // Read DAIF into flags
            "msr daifset, #2",   // Disable IRQs (set the I bit)
            out(reg) flags,
            options(nomem, nostack)
        );
    }
    flags
}

/// Restores the interrupt flags to a previously saved state.
#[inline(always)]
pub fn local_irq_restore(flags: u64) {
    unsafe {
        asm!(
            "msr daif, {0}",    // Write flags back to DAIF
            in(reg) flags,
            options(nomem, nostack)
        );
    }
}
