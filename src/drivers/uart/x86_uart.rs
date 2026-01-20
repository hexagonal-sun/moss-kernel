use crate::{
    console::Console,
    drivers::{Driver, uart::{Uart, UartDriver}},
    sync::SpinLock,
};
use core::arch::asm;
use alloc::{sync::Arc, boxed::Box, format};
use crate::sync::OnceLock;

pub struct X86Uart {
    port: u16,
}

impl X86Uart {
    pub const fn new(port: u16) -> Self {
        Self { port }
    }

    pub fn init(&self) {
        unsafe {
            outb(self.port + 1, 0x00);    // Disable all interrupts
            outb(self.port + 3, 0x80);    // Enable DLAB (set baud rate divisor)
            outb(self.port + 0, 0x01);    // Set divisor to 1 (lo byte) 115200 baud
            outb(self.port + 1, 0x00);    //                  (hi byte)
            outb(self.port + 3, 0x03);    // 8 bits, no parity, one stop bit
            outb(self.port + 2, 0xC7);    // Enable FIFO, clear them, with 14-byte threshold
            outb(self.port + 4, 0x0B);    // IRQs enabled, RTS/DSR set
        }
    }

    fn is_transmit_empty(&self) -> bool {
        unsafe { inb(self.port + 5) & 0x20 != 0 }
    }

    pub fn write_byte(&self, b: u8) {
        if b == b'\n' {
            self.write_byte(b'\r');
        }
        while !self.is_transmit_empty() {}
        unsafe { outb(self.port, b); }
    }
}

impl core::fmt::Write for X86Uart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.as_bytes() {
            self.write_byte(*b);
        }
        Ok(())
    }
}

impl UartDriver for X86Uart {
    fn write_buf(&mut self, buf: &[u8]) {
        for b in buf {
            self.write_byte(*b);
        }
    }

    fn drain_uart_rx(&mut self, buf: &mut [u8]) -> usize {
        let mut count = 0;
        while count < buf.len() {
            if unsafe { inb(self.port + 5) & 1 == 0 } {
                break;
            }
            buf[count] = unsafe { inb(self.port) };
            count += 1;
        }
        count
    }
}

unsafe fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

static EARLY_UART_CONSOLE: OnceLock<Arc<X86UartConsole>> = OnceLock::new();

// Manual initialization for early console
pub fn early_x86_uart_init() {
    let uart = X86Uart::new(0x3f8);
    uart.init();

    let console = Arc::new(X86UartConsole {
        inner: SpinLock::new(uart),
    });

    EARLY_UART_CONSOLE.set(console.clone()).ok();

    let desc = libkernel::driver::CharDevDescriptor {
        major: crate::drivers::ReservedMajors::Uart as _,
        minor: 0,
    };

    // Register as a char driver so /dev/console can find it
    crate::drivers::DM
        .lock_save_irq()
        .register_char_driver(desc.major, console.clone())
        .expect("Failed to register UART char driver");

    crate::console::set_active_console(console, desc).expect("Failed to set active console");
}

pub struct X86UartConsole {
    inner: SpinLock<X86Uart>,
}

impl Console for X86UartConsole {
    fn write_char(&self, c: char) {
        self.inner.lock_save_irq().write_byte(c as u8);
    }

    fn write_fmt(&self, args: core::fmt::Arguments) -> core::fmt::Result {
        use core::fmt::Write;
        self.inner.lock_save_irq().write_fmt(args)
    }

    fn write_buf(&self, buf: &[u8]) {
        self.inner.lock_save_irq().write_buf(buf);
    }

    fn register_input_handler(&self, _handler: alloc::sync::Weak<dyn crate::console::tty::TtyInputHandler>) {
        // Not implemented for early console
    }
}

impl crate::drivers::CharDriver for X86UartConsole {
    fn get_device(&self, minor: u64) -> Option<Arc<dyn crate::drivers::OpenableDevice>> {
        if minor == 0 {
            Some(Arc::new(X86UartInstance {
                console: EARLY_UART_CONSOLE.get().expect("UART not initialized").clone(),
            }))
        } else {
            None
        }
    }
}

struct X86UartInstance {
    console: Arc<X86UartConsole>,
}

impl crate::drivers::OpenableDevice for X86UartInstance {
    fn open(&self, flags: libkernel::fs::OpenFlags) -> libkernel::error::Result<Arc<crate::fs::open_file::OpenFile>> {
        use crate::console::tty::Tty;
        let tty = Tty::new(self.console.clone())?;
        Ok(Arc::new(crate::fs::open_file::OpenFile::new(Box::new(tty), flags)))
    }
}
