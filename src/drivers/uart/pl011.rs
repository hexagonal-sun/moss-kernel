use crate::{
    arch::ArchImpl,
    drivers::{
        DeviceDescriptor, Driver, DriverManager,
        init::PlatformBus,
        probe::{DeviceMatchType, FdtFlags},
    },
    kernel_driver,
};
use alloc::{boxed::Box, sync::Arc};
use arm_pl011_uart::{
    DataBits, Interrupts, LineConfig, PL011Registers, Parity, StopBits, UniqueMmioPointer,
};
use core::ptr::NonNull;
use libkernel::{
    KernAddressSpace, VirtualMemory,
    error::{ProbeError, Result},
    memory::{
        address::{PA, VA},
        region::PhysMemoryRegion,
    },
};

use super::{UART_CHAR_DEV, Uart, UartDriver};

pub struct PL011 {
    inner: arm_pl011_uart::Uart<'static>,
}

impl PL011 {
    pub fn new(base_addr: VA) -> Self {
        let ptr = unsafe {
            UniqueMmioPointer::new(NonNull::new_unchecked(
                base_addr.as_ptr_mut().cast::<PL011Registers>(),
            ))
        };

        let mut uart = arm_pl011_uart::Uart::new(ptr);

        let line_config = LineConfig {
            data_bits: DataBits::Bits8,
            parity: Parity::None,
            stop_bits: StopBits::One,
        };

        uart.enable(line_config, 115_200, 16_000_000).unwrap();

        // Interrupts not enabled yet, just mask RX interrupts in hardware
        uart.set_interrupt_masks(Interrupts::RXI);

        Self { inner: uart }
    }
}

impl core::fmt::Write for PL011 {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write_buf(s.as_bytes());

        Ok(())
    }
}

impl UartDriver for PL011 {
    fn write_buf(&mut self, buf: &[u8]) {
        for c in buf {
            self.inner.write_word(*c);
        }
    }

    fn drain_uart_rx(&mut self, buf: &mut [u8]) -> usize {
        self.inner.clear_interrupts(Interrupts::RXI);
        let mut bytes_read = 0;

        while !self.inner.is_rx_fifo_empty() && bytes_read < buf.len() {
            if let Ok(Some(byte)) = self.inner.read_word() {
                buf[bytes_read] = byte;
                bytes_read += 1;
            } else {
                break;
            }
        }

        bytes_read
    }
}

pub fn pl011_probe(dm: &mut DriverManager, d: DeviceDescriptor) -> Result<Arc<dyn Driver>> {
    match d {
        DeviceDescriptor::Fdt(fdt_node, flags) => {
            let region = fdt_node
                .reg()
                .ok_or(ProbeError::NoReg)?
                .next()
                .ok_or(ProbeError::NoReg)?;

            let size = region.size.ok_or(ProbeError::NoRegSize)?;

            let mut interrupts = fdt_node
                .interrupts()
                .ok_or(ProbeError::NoInterrupts)?
                .next()
                .ok_or(ProbeError::NoInterrupts)?;

            let interrupt_node = fdt_node
                .interrupt_parent()
                .ok_or(ProbeError::NoParentInterrupt)?
                .node;

            let interrupt_manager = dm
                .find_by_name(interrupt_node.name)
                .ok_or(ProbeError::Deferred)?
                .as_interrupt_manager()
                .ok_or(ProbeError::NotInterruptController)?;

            let uart_cdev = UART_CHAR_DEV.get().ok_or(ProbeError::Deferred)?;

            let mem =
                ArchImpl::kern_address_space()
                    .lock_save_irq()
                    .map_mmio(PhysMemoryRegion::new(
                        PA::from_value(region.address as usize),
                        size,
                    ))?;

            let interrupt_config = interrupt_manager.parse_fdt_interrupt_regs(&mut interrupts)?;

            let dev = interrupt_manager.claim_interrupt(interrupt_config, |claimed_interrupt| {
                Uart::new(PL011::new(mem), claimed_interrupt, fdt_node.name)
            })?;

            uart_cdev.register_console(dev.clone(), flags.contains(FdtFlags::ACTIVE_CONSOLE))?;

            Ok(dev)
        }
    }
}

pub fn pl011_init(bus: &mut PlatformBus, _dm: &mut DriverManager) -> Result<()> {
    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("arm,pl011"),
        Box::new(pl011_probe),
    );

    Ok(())
}

kernel_driver!(pl011_init);
