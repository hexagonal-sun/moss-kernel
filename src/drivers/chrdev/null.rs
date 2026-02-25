use crate::{
    drivers::{
        CharDriver, DriverManager, OpenableDevice, ReservedMajors, fs::dev::devfs,
        init::PlatformBus,
    },
    fs::{fops::FileOps, open_file::OpenFile},
    kernel_driver,
};
use alloc::string::ToString;
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use core::{future::Future, pin::Pin};
use libkernel::{
    driver::CharDevDescriptor,
    error::Result,
    fs::{OpenFlags, attr::FilePermissions},
    memory::address::UA,
};

/// `/dev/null` file operations.
struct NullFileOps;

#[async_trait]
impl FileOps for NullFileOps {
    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        // EOF
        Ok(0)
    }

    async fn writeat(&mut self, _buf: UA, count: usize, _offset: u64) -> Result<usize> {
        // Pretend we wrote everything successfully.
        Ok(count)
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        // Always ready to read (but will return EOF).
        Box::pin(async { Ok(()) })
    }

    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        // Always ready to accept writes.
        Box::pin(async { Ok(()) })
    }
}

struct NullDev;

impl OpenableDevice for NullDev {
    fn open(&self, flags: OpenFlags) -> Result<Arc<OpenFile>> {
        Ok(Arc::new(OpenFile::new(Box::new(NullFileOps), flags)))
    }
}

struct NullCharDev {
    null_dev: Arc<dyn OpenableDevice>,
}

impl NullCharDev {
    fn new() -> Result<Self> {
        // Register the /dev/null node in devfs.
        devfs().mknod(
            "null".to_string(),
            CharDevDescriptor {
                major: ReservedMajors::Null as _,
                minor: 0,
            },
            // World-writable, world-readable like on Linux.
            FilePermissions::from_bits_retain(0o666),
        )?;

        Ok(Self {
            null_dev: Arc::new(NullDev),
        })
    }
}

impl CharDriver for NullCharDev {
    fn get_device(&self, minor: u64) -> Option<Arc<dyn OpenableDevice>> {
        if minor == 0 {
            Some(self.null_dev.clone())
        } else {
            None
        }
    }
}

pub fn null_chardev_init(_bus: &mut PlatformBus, dm: &mut DriverManager) -> Result<()> {
    let cdev = NullCharDev::new()?;
    dm.register_char_driver(ReservedMajors::Null as _, Arc::new(cdev))
}

kernel_driver!(null_chardev_init);
