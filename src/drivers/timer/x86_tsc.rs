use crate::drivers::Driver;
use crate::drivers::timer::{HwTimer, Instant};
use core::arch::asm;

pub struct X86TscTimer {
    freq: u64,
}

impl X86TscTimer {
    pub fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            // Simple assumption for QEMU/Test
            Self { freq: 2_000_000_000 }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            Self { freq: 1 }
        }
    }
}

impl Driver for X86TscTimer {
    fn name(&self) -> &'static str { "x86_tsc" }
}

impl HwTimer for X86TscTimer {
    fn now(&self) -> Instant {
        let low: u32;
        let high: u32;
        unsafe {
            asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack));
        }
        let ticks = ((high as u64) << 32) | (low as u64);
        Instant { ticks, freq: self.freq }
    }

    fn schedule_interrupt(&self, _when: Option<Instant>) {
        // No-op: Preemption will not work, but we are just testing syscalls.
    }
}
