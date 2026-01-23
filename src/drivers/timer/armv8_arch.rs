use core::time::Duration;

use aarch64_cpu::registers::{
    CNTFRQ_EL0, CNTP_CTL_EL0, CNTP_CVAL_EL0, CNTPCT_EL0, Readable, Writeable,
};
use alloc::{boxed::Box, sync::Arc};
use libkernel::error::Result;
use log::warn;

use crate::{
    drivers::{
        Driver, DriverManager,
        init::PlatformBus,
        probe::{DeviceDescriptor, DeviceMatchType},
        timer::{SYS_TIMER, SysTimer},
    },
    interrupts::{ClaimedInterrupt, InterruptDescriptor::Ppi},
    kernel_driver,
};

use super::{HwTimer, Instant};

struct Armv8Timer {
    fdt_name: Option<&'static str>,
    freq: u64,
    _interrupt: ClaimedInterrupt,
}

impl Driver for Armv8Timer {
    fn name(&self) -> &'static str {
        self.fdt_name.unwrap_or("ARMv8 Architectural Timer")
    }
}

impl HwTimer for Armv8Timer {
    fn now(&self) -> Instant {
        Instant {
            ticks: CNTPCT_EL0.get(),
            freq: self.freq,
        }
    }

    fn schedule_interrupt(&self, when: Option<Instant>) {
        if let Some(instant) = when {
            CNTP_CVAL_EL0.set(instant.ticks);
            CNTP_CTL_EL0.write(CNTP_CTL_EL0::ENABLE::SET + CNTP_CTL_EL0::IMASK::CLEAR);
        } else {
            CNTP_CTL_EL0.write(CNTP_CTL_EL0::ENABLE::CLEAR);
        }
    }
}

fn armv8_timer_probe(dm: &mut DriverManager, d: DeviceDescriptor) -> Result<Arc<dyn Driver>> {
    match d {
        DeviceDescriptor::Fdt(fdt_node, _) => {
            use libkernel::error::ProbeError::*;

            let interrupt_node = fdt_node.interrupt_parent().ok_or(NoParentInterrupt)?.node;

            let interrupt_manager = dm
                .find_by_name(interrupt_node.name)
                .ok_or(Deferred)?
                .as_interrupt_manager()
                .ok_or(NotInterruptController)?;

            let freq = CNTFRQ_EL0.get();

            let mut el1_phys_timer_interrupt = None;

            for mut interrupt in fdt_node.interrupts().ok_or(NoInterrupts)? {
                let int_desc = interrupt_manager.parse_fdt_interrupt_regs(&mut interrupt)?;
                if matches!(int_desc.descriptor, Ppi(14)) {
                    el1_phys_timer_interrupt = Some(int_desc);
                    break;
                }
            }

            let interrupt = el1_phys_timer_interrupt.ok_or(NoInterrupts)?;

            let sys_timer = interrupt_manager.claim_interrupt(interrupt, |claimed_interrupt| {
                let base_driver = Arc::new(Armv8Timer {
                    fdt_name: Some(fdt_node.name),
                    _interrupt: claimed_interrupt,
                    freq,
                });

                base_driver.schedule_interrupt(Some(base_driver.now() + Duration::from_secs(5)));

                SysTimer::from_driver(base_driver)
            })?;

            if SYS_TIMER.set(sys_timer.clone()).is_err() {
                warn!("Failed to set system timer");
            }

            Ok(sys_timer)
        }
    }
}

pub fn armv8_timer_init(bus: &mut PlatformBus, _dm: &mut DriverManager) -> Result<()> {
    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("arm,armv8-timer"),
        Box::new(armv8_timer_probe),
    );

    Ok(())
}

kernel_driver!(armv8_timer_init);
