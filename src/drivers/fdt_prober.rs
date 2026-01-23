use super::{DM, DeviceDescriptor, init::PLATFORM_BUS, probe::FdtFlags};
use alloc::vec::Vec;
use core::ptr::NonNull;
use fdt_parser::Fdt;
use libkernel::{
    error::{KernelError, ProbeError},
    memory::address::TVA,
};
use log::{error, warn};

static mut FDT: TVA<u8> = TVA::from_value(usize::MAX);

pub fn set_fdt_va(fdt: TVA<u8>) {
    unsafe { FDT = fdt };
}

pub fn get_fdt() -> Fdt<'static> {
    if unsafe { FDT.value() } == usize::MAX {
        panic!("Attempted to access FDT VA before set");
    }

    unsafe { Fdt::from_ptr(NonNull::new_unchecked(FDT.as_ptr_mut())).unwrap() }
}

pub fn is_intc_root(node: &fdt_parser::Node) -> bool {
    assert!(node.find_property("interrupt-controller").is_some());

    match node.interrupt_parent() {
        Some(n) if n.node.name == node.name => true,
        None => true,
        _ => false,
    }
}

pub fn probe_for_fdt_devices() {
    let fdt = get_fdt();
    let mut driver_man = DM.lock_save_irq();
    let platform_bus = PLATFORM_BUS.lock_save_irq();

    let mut to_probe: Vec<_> = fdt
        .all_nodes()
        .filter(|node| {
            // Pre-filter nodes that are disabled, already probed, or have no
            // compatible string.
            driver_man.find_by_name(node.name).is_none()
                && node.status().unwrap_or(fdt_parser::Status::Okay) == fdt_parser::Status::Okay
                && node.compatible().is_some()
        })
        .map(|node| {
            let flags = if is_active_console(node.name) {
                FdtFlags::ACTIVE_CONSOLE
            } else {
                FdtFlags::empty()
            };

            DeviceDescriptor::Fdt(node, flags)
        })
        .collect();

    let mut deferred_list = Vec::new();
    let mut progress_made = true;

    // Loop as long as we are successfully probing drivers.
    while progress_made {
        progress_made = false;
        deferred_list.clear();

        for desc in to_probe.drain(..) {
            match platform_bus.probe_device(&mut driver_man, desc.clone()) {
                Ok(Some(_)) => {
                    progress_made = true;
                }
                Err(KernelError::Probe(ProbeError::Deferred)) => {
                    deferred_list.push(desc);
                }
                Ok(None) => {
                    // No driver found for this compatible string. Not an error, just ignore.
                }
                Err(e) => {
                    // A fatal error occurred during probe.
                    error!("Fatal error while probing device \"{desc}\": {e}");
                }
            }
        }

        // The next iteration of the loop will only try the devices that were deferred.
        to_probe.append(&mut deferred_list);

        // If we made no progress in a full pass, we are done (or have an unresolvable dependency).
        if !progress_made && !to_probe.is_empty() {
            for desc in &to_probe {
                warn!("Could not probe device \"{desc}\" due to missing dependencies.");
            }
            break;
        }
    }
}

pub fn is_active_console(name: &'static str) -> bool {
    let fdt = get_fdt();

    fdt.chosen()
        .and_then(|chosen| chosen.stdout())
        .map(|stdout| stdout.node.name == name)
        .unwrap_or(false)
}
