use super::{
    Driver, DriverManager,
    block::register_block_device,
    probe::{DeviceDescriptor, DeviceMatchType, ProbeFn},
};
use crate::{drivers::DM, sync::SpinLock};
use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use libkernel::error::{KernelError, ProbeError, Result};
use log::error;

pub type InitFunc = fn(&mut PlatformBus, &mut DriverManager) -> Result<()>;

pub struct PlatformBus {
    probers: BTreeMap<DeviceMatchType, Vec<ProbeFn>>,
}

impl PlatformBus {
    pub const fn new() -> Self {
        Self {
            probers: BTreeMap::new(),
        }
    }

    /// Called by driver `init` functions to register their ability to probe for
    /// certain hardware.
    pub fn register_platform_driver(&mut self, match_type: DeviceMatchType, probe_fn: ProbeFn) {
        self.probers.entry(match_type).or_default().push(probe_fn);
    }

    /// Called by the FDT prober to find the right driver and probe.
    pub fn probe_device(
        &self,
        dm: &mut DriverManager,
        descr: DeviceDescriptor,
    ) -> Result<Option<Arc<dyn Driver>>> {
        let matcher = match &descr {
            DeviceDescriptor::Fdt(node, _) => {
                // Find the first compatible string that we have a driver for.
                node.compatible().and_then(|compats| {
                    for compat in compats {
                        let compat_str = compat.ok()?;

                        let match_type = DeviceMatchType::FdtCompatible(compat_str);

                        if self.probers.contains_key(&match_type) {
                            return Some(match_type);
                        }
                    }

                    None
                })
            }
        };

        if let Some(match_type) = matcher
            && let Some(probe_fns) = self.probers.get(&match_type)
        {
            // Try each registered probe function until one claims the device.
            for probe_fn in probe_fns {
                match (probe_fn)(dm, descr.clone()) {
                    Ok(driver) => {
                        if let Some(block_device) = driver.clone().as_block_device() {
                            register_block_device(driver.name(), block_device);
                        }

                        dm.insert_driver(driver.clone());
                        return Ok(Some(driver));
                    }
                    Err(KernelError::Probe(ProbeError::NoMatch)) => {
                        // This driver doesn't want this device, try next.
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            // All probe functions returned NoMatch.
            return Err(KernelError::Probe(ProbeError::NoMatch));
        }

        Ok(None)
    }
}

/// Run all init calls for all internal kernel drivers.
///
/// SAFETY: The function should only be called once during boot.
pub unsafe fn run_initcalls() {
    unsafe extern "C" {
        static __driver_inits_start: u8;
        static __driver_inits_end: u8;
    }

    unsafe {
        let start = &__driver_inits_start as *const _ as *const InitFunc;
        let end = &__driver_inits_end as *const _ as *const InitFunc;
        let mut current = start;

        let mut bus = PLATFORM_BUS.lock_save_irq();
        let mut dm = DM.lock_save_irq();

        while current < end {
            let init_func = &*current;
            // Call each driver's init function
            if let Err(e) = init_func(&mut bus, &mut dm) {
                error!("A driver failed to initialize: {e}");
            }

            current = current.add(1);
        }
    }
}

#[macro_export]
macro_rules! kernel_driver {
    ($init_func:expr) => {
        paste::paste! {
            #[unsafe(no_mangle)]
            #[unsafe(link_section = ".driver_inits")]
            #[used(linker)]
            static [<DRIVER_INIT_ $init_func>]: $crate::drivers::init::InitFunc = $init_func;
        }
    };
}

pub static PLATFORM_BUS: SpinLock<PlatformBus> = SpinLock::new(PlatformBus::new());
