//! A generic UART driver framework.
//!
//! This module provides a two-layered architecture for UART drivers in the
//! kernel. The goal is to separate the hardware-specific register manipulation
//! from the high-level, hardware-agnostic tasks like console integration,
//! interrupt handling, and TTY input forwarding.
//!
//! # Architecture
//!
//! 1. `UartDriver`: A low-level trait that defines the minimal, direct hardware
//!    operations required for a UART device. Implementors of this trait will
//!    write the code that reads from and writes to the hardware's memory-mapped
//!    registers.
//!
//! 2. `Uart<D>` Struct: A high-level, generic struct that wraps a concrete `UartDriver`
//!    implementation. It handles all the common boilerplate:
//!    - Implementing the `Console` trait for `printk!`-style formatted output.
//!    - Implementing the `InterruptHandler` trait to read incoming bytes.
//!    - Interfacing with a TTY layer via `TtyInputHandler`.
//!
//! 3.  `UartCharDev`: A UART character device, responsible for registering the
//!     UART as a char device to obtain a `DriverDescriptor`. Also exposes the
//!     device to userspace via `devfs`.

use core::sync::atomic::{AtomicU64, Ordering};

use super::{
    CharDriver, Driver, DriverManager, OpenableDevice, ReservedMajors, fs::dev::devfs,
    init::PlatformBus,
};
use crate::{
    console::{
        Console, set_active_console,
        tty::{Tty, TtyInputHandler},
    },
    fs::open_file::OpenFile,
    interrupts::{ClaimedInterrupt, InterruptHandler},
    kernel_driver,
    sync::{OnceLock, SpinLock},
};
use alloc::{
    boxed::Box,
    collections::btree_map::{BTreeMap, Entry},
    format,
    sync::{Arc, Weak},
};
use libkernel::{
    driver::CharDevDescriptor,
    error::{KernelError, Result},
    fs::{OpenFlags, attr::FilePermissions},
};

//pub mod bcm2835_aux;
pub mod imx_lp;
#[cfg(target_arch = "x86_64")]
pub mod x86_uart;
pub mod pl011;


/// A trait for low-level, hardware-specific UART drivers.
///
/// Implementors of this trait are responsible for the direct hardware
/// manipulation required to send and receive bytes. The methods are designed to
/// be called from a higher-level context that handles locking and interrupt
/// management.
///
/// In addition to this trait, a driver must also implement `core::fmt::Write`
/// to be used with the generic `Uart` wrapper.
pub trait UartDriver: core::fmt::Write + Send + Sync + 'static {
    /// Writes a raw byte slice to the UART's transmit buffer.
    ///
    /// This method should block until all bytes in the buffer have been
    /// successfully written to the hardware's transmit FIFO.
    fn write_buf(&mut self, buf: &[u8]);

    /// Reads all available bytes from the UART's receive buffer into `buf`.
    ///
    /// This method should read from the hardware's receive FIFO until it is
    /// empty or until the provided `buf` is full. It must not block if the FIFO
    /// is empty.
    ///
    /// # Returns
    ///
    /// The number of bytes that were actually read from the FIFO and written
    /// into `buf`.
    fn drain_uart_rx(&mut self, buf: &mut [u8]) -> usize;
}

/// A generic, high-level UART device.
///
/// This struct acts as a wrapper around a concrete `UartDriver` implementation,
/// providing common high-level functionality such as console integration,
/// interrupt handling, and TTY input routing.
pub struct Uart<D: UartDriver> {
    driver: SpinLock<D>,
    name: &'static str,
    _interrupt: ClaimedInterrupt,
    tty_handler: SpinLock<Option<Weak<dyn TtyInputHandler>>>,
}

impl<D: UartDriver> Console for Uart<D> {
    fn write_char(&self, c: char) {
        let _ = self.driver.lock_save_irq().write_char(c);
    }

    fn write_fmt(&self, args: core::fmt::Arguments) -> core::fmt::Result {
        self.driver.lock_save_irq().write_fmt(args)
    }

    fn write_buf(&self, buf: &[u8]) {
        self.driver.lock_save_irq().write_buf(buf);
    }

    fn register_input_handler(&self, handler: Weak<dyn TtyInputHandler>) {
        *self.tty_handler.lock_save_irq() = Some(handler);
    }
}

impl<D: UartDriver> Uart<D> {
    /// Creates a new high-level `Uart` instance.
    ///
    /// # Arguments
    ///
    /// * `driver`: An initialized instance of the concrete, low-level UART
    ///   driver.
    /// * `interrupt`: The `ClaimedInterrupt` resource for this UART's IRQ.
    /// * `name`: A static string-slice to identify this device.
    pub fn new(driver: D, interrupt: ClaimedInterrupt, name: &'static str) -> Self {
        Self {
            driver: SpinLock::new(driver),
            name,
            _interrupt: interrupt,
            tty_handler: SpinLock::new(None),
        }
    }
}

impl<D: UartDriver> Driver for Uart<D> {
    fn name(&self) -> &'static str {
        self.name
    }
}

/// Handles incoming interrupts from the UART hardware.
impl<D: UartDriver> InterruptHandler for Uart<D> {
    /// The interrupt handler function.
    ///
    /// The handler drains the UART's receive FIFO and forwards the bytes to the
    /// registered TTY input handler.
    fn handle_irq(&self, _desc: crate::interrupts::InterruptDescriptor) {
        const BUF_CAPACITY: usize = 32;
        let mut byte_buf = [0u8; BUF_CAPACITY];

        // Drain phase: Lock the driver and call its drain method.
        let bytes_read = self.driver.lock_save_irq().drain_uart_rx(&mut byte_buf);

        // Processing phase: If bytes were read, forward them to the TTY.
        if bytes_read > 0
            && let Some(handler) = self
                .tty_handler
                .lock_save_irq()
                .as_ref()
                .and_then(|h| h.upgrade())
        {
            // Push each received byte to the TTY input queue.
            byte_buf
                .into_iter()
                .take(bytes_read)
                .for_each(|b| handler.push_byte(b))
        }
    }
}

struct UartInstance {
    driver: Arc<dyn Console>,
}

impl OpenableDevice for UartInstance {
    fn open(&self, flags: OpenFlags) -> Result<Arc<OpenFile>> {
        let tty = Tty::new(self.driver.clone())?;

        Ok(Arc::new(OpenFile::new(Box::new(tty), flags)))
    }
}

pub struct UartCharDev {
    next_instance: AtomicU64,
    instancies: SpinLock<BTreeMap<u64, Arc<dyn OpenableDevice>>>,
}

impl CharDriver for UartCharDev {
    fn get_device(&self, minor: u64) -> Option<Arc<dyn OpenableDevice>> {
        self.instancies.lock_save_irq().get(&minor).cloned()
    }
}

impl UartCharDev {
    fn allocate_minor(&self) -> u64 {
        self.next_instance.fetch_add(1, Ordering::SeqCst)
    }

    fn register_console(
        &self,
        driver: Arc<dyn Console>,
        active_console: bool,
    ) -> Result<CharDevDescriptor> {
        let minor = self.allocate_minor();

        let desc = CharDevDescriptor {
            major: ReservedMajors::Uart as _,
            minor,
        };

        match self.instancies.lock_save_irq().entry(minor) {
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(Arc::new(UartInstance {
                    driver: driver.clone(),
                }));

                devfs().mknod(
                    format!("ttyS{}", minor),
                    desc,
                    FilePermissions::from_bits_retain(0o600),
                )?;

                if active_console {
                    set_active_console(driver, desc)?;
                }

                Ok(desc)
            }
            Entry::Occupied(_) => Err(KernelError::InUse),
        }
    }
}

pub fn uart_init(_bus: &mut PlatformBus, dm: &mut DriverManager) -> Result<()> {
    let cdev = Arc::new(UartCharDev {
        next_instance: AtomicU64::new(0),
        instancies: SpinLock::new(BTreeMap::new()),
    });

    UART_CHAR_DEV
        .set(cdev.clone())
        .map_err(|_| KernelError::InUse)?;

    dm.register_char_driver(ReservedMajors::Uart as _, cdev)
}

static UART_CHAR_DEV: OnceLock<Arc<UartCharDev>> = OnceLock::new();

kernel_driver!(uart_init);
