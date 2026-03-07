use crate::arch::ArchImpl;
use crate::drivers::init::PlatformBus;
use crate::drivers::probe::{DeviceDescriptor, DeviceMatchType};
use crate::drivers::rtc::{Rtc, set_rtc_driver};
use crate::drivers::{Driver, DriverManager};
use crate::kernel_driver;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::time::Duration;
use libkernel::error::{ProbeError, Result};
use libkernel::memory::address::{PA, VA};
use libkernel::memory::region::PhysMemoryRegion;
use libkernel::{KernAddressSpace, VirtualMemory};

/// Driver for a PL031 real-time clock.
pub struct PL031 {
    inner: arm_pl031::Rtc,
}

impl PL031 {
    /// Constructs a new instance of the RTC driver for a PL031 device with the
    /// given base address.
    pub fn new(base_addr: VA) -> Self {
        let rtc = unsafe { arm_pl031::Rtc::new(base_addr.as_ptr_mut() as _) };
        Self { inner: rtc }
    }
}

impl Rtc for PL031 {
    fn time(&self) -> Option<Duration> {
        Some(Duration::new(self.inner.get_unix_timestamp() as u64, 0))
    }

    fn set_time(&mut self, time: Duration) -> libkernel::error::Result<()> {
        self.inner.set_unix_timestamp(time.as_secs() as _);
        Ok(())
    }
}

impl Driver for PL031 {
    fn name(&self) -> &'static str {
        "ARM PrimeCell Real Time Clock"
    }
}

pub fn pl031_probe(_dm: &mut DriverManager, d: DeviceDescriptor) -> Result<Arc<dyn Driver>> {
    match d {
        DeviceDescriptor::Fdt(fdt_node, _flags) => {
            let region = fdt_node
                .reg()
                .ok_or(ProbeError::NoReg)?
                .next()
                .ok_or(ProbeError::NoReg)?;

            let size = region.size.ok_or(ProbeError::NoRegSize)?;

            let mem =
                ArchImpl::kern_address_space()
                    .lock_save_irq()
                    .map_mmio(PhysMemoryRegion::new(
                        PA::from_value(region.address as usize),
                        size,
                    ))?;

            let dev = Arc::new(PL031::new(mem));
            set_rtc_driver(dev.clone());
            Ok(dev)
        }
    }
}

pub fn pl031_init(bus: &mut PlatformBus, _dm: &mut DriverManager) -> Result<()> {
    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("arm,pl031"),
        Box::new(pl031_probe),
    );

    Ok(())
}

kernel_driver!(pl031_init);
