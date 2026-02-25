use core::{
    any::Any,
    sync::atomic::{AtomicU64, Ordering},
};

use alloc::{
    collections::btree_map::{BTreeMap, Entry},
    sync::Arc,
    vec::Vec,
};
use libkernel::{
    error::{KernelError, Result},
    fs::OpenFlags,
};
use probe::DeviceDescriptor;

use crate::{
    fs::{FilesystemDriver, open_file::OpenFile},
    interrupts::InterruptManager,
    sync::SpinLock,
};

pub mod chrdev;
pub mod fdt_prober;
pub mod fs;
pub mod init;
pub mod interrupts;
pub mod probe;
pub mod timer;
pub mod uart;

#[repr(u64)]
pub enum ReservedMajors {
    Null = 1,
    Zero = 2,
    Random = 3,
    Console = 5,
    Uart = 10,
    End = 11,
}

pub trait Driver: Send + Sync + Any {
    fn name(&self) -> &'static str;

    fn as_interrupt_manager(self: Arc<Self>) -> Option<Arc<InterruptManager>> {
        None
    }

    fn as_filesystem_driver(self: Arc<Self>) -> Option<Arc<dyn FilesystemDriver>> {
        None
    }
}

pub trait OpenableDevice: Send + Sync {
    fn open(&self, args: OpenFlags) -> Result<Arc<OpenFile>>;
}

/// A driver that should be exposed to userspace via the VFS.
pub trait CharDriver: Send + Sync + 'static {
    /// Given a minor number, this function creates the Inode for that specific
    /// device instance. It can fail if the minor number is invalid for this
    /// driver.
    fn get_device(&self, minor: u64) -> Option<Arc<dyn OpenableDevice>>;
}

pub struct DriverManager {
    /// Every driver instance in the system.
    active_drivers: Vec<Arc<dyn Driver>>,
    _next_major: AtomicU64,
    /// Maps a major number to an instance of a CharDriver.
    char_drivers: BTreeMap<u64, Arc<dyn CharDriver>>,
}

impl DriverManager {
    pub const fn new() -> Self {
        Self {
            active_drivers: Vec::new(),
            _next_major: AtomicU64::new(ReservedMajors::End as _),
            char_drivers: BTreeMap::new(),
        }
    }

    pub fn insert_driver(&mut self, driver: Arc<dyn Driver>) {
        self.active_drivers.push(driver);
    }

    pub fn find_by_name(&self, name: &str) -> Option<Arc<dyn Driver>> {
        self.active_drivers.iter().find_map(|drv| {
            if drv.name() == name {
                Some(drv.clone())
            } else {
                None
            }
        })
    }

    pub fn _allocate_major(&self) -> u64 {
        self._next_major.fetch_add(1, Ordering::SeqCst)
    }

    pub fn register_char_driver(&mut self, major: u64, driver: Arc<dyn CharDriver>) -> Result<()> {
        match self.char_drivers.entry(major) {
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(driver);
                Ok(())
            }
            Entry::Occupied(_) => Err(KernelError::InUse),
        }
    }

    pub fn find_char_driver(&self, major: u64) -> Option<Arc<dyn CharDriver>> {
        self.char_drivers.get(&major).cloned()
    }
}

pub static DM: SpinLock<DriverManager> = SpinLock::new(DriverManager::new());
