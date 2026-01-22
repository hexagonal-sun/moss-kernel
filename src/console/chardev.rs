use crate::{
    drivers::{
        CharDriver, DM, DriverManager, OpenableDevice, ReservedMajors, fs::dev::devfs,
        init::PlatformBus,
    },
    fs::open_file::OpenFile,
    kernel_driver,
    process::fd_table::Fd,
    sched::current::current_task,
};
use alloc::{string::ToString, sync::Arc};
use libkernel::{
    driver::CharDevDescriptor,
    error::{FsError, Result},
    fs::{OpenFlags, attr::FilePermissions},
};

use super::CONSOLE;

struct TtyDev {}

impl OpenableDevice for TtyDev {
    fn open(&self, _args: OpenFlags) -> Result<Arc<OpenFile>> {
        let task = current_task();

        // TODO: This should really open the controlling terminal of the
        // session.
        Ok(task
            .fd_table
            .lock_save_irq()
            .get(Fd(0))
            .ok_or(FsError::NoDevice)?)
    }
}

struct ConsoleDev {}

impl OpenableDevice for ConsoleDev {
    fn open(&self, flags: OpenFlags) -> Result<Arc<OpenFile>> {
        let char_dev_desc = match *CONSOLE.lock_save_irq() {
            super::ConsoleState::Buffered => return Err(FsError::NoDevice.into()),
            super::ConsoleState::Device(_, char_dev_descriptor) => char_dev_descriptor,
        };

        let char_driver = DM
            .lock_save_irq()
            .find_char_driver(char_dev_desc.major)
            .ok_or(FsError::NoDevice)?;

        char_driver
            .get_device(char_dev_desc.minor)
            .ok_or(FsError::NoDevice)?
            .open(flags)
    }
}

struct ConsoleCharDev {
    tty_dev: Arc<dyn OpenableDevice>,
    console_dev: Arc<dyn OpenableDevice>,
}

impl ConsoleCharDev {
    pub fn new() -> Result<Self> {
        devfs().mknod(
            "console".to_string(),
            CharDevDescriptor {
                major: ReservedMajors::Console as _,
                minor: 1,
            },
            FilePermissions::from_bits_retain(0o600),
        )?;

        devfs().mknod(
            "tty".to_string(),
            CharDevDescriptor {
                major: ReservedMajors::Console as _,
                minor: 0,
            },
            FilePermissions::from_bits_retain(0o600),
        )?;

        Ok(Self {
            tty_dev: Arc::new(TtyDev {}),
            console_dev: Arc::new(ConsoleDev {}),
        })
    }
}

impl CharDriver for ConsoleCharDev {
    fn get_device(&self, minor: u64) -> Option<Arc<dyn OpenableDevice>> {
        match minor {
            0 => Some(self.tty_dev.clone()),
            1 => Some(self.console_dev.clone()),
            _ => None,
        }
    }
}

pub fn console_chardev_init(_bus: &mut PlatformBus, dm: &mut DriverManager) -> Result<()> {
    let ccd = ConsoleCharDev::new()?;

    dm.register_char_driver(ReservedMajors::Console as _, Arc::new(ccd))?;

    Ok(())
}

kernel_driver!(console_chardev_init);
