use crate::{
    drivers::{
        CharDriver, DriverManager, OpenableDevice, ReservedMajors, fs::dev::devfs,
        init::PlatformBus,
    },
    fs::{
        fops::FileOps,
        open_file::{FileCtx, OpenFile},
    },
    kernel::rand::sys_getrandom,
    kernel_driver,
};
use alloc::{boxed::Box, string::ToString, sync::Arc};
use async_trait::async_trait;
use core::{future::Future, pin::Pin};
use libkernel::{
    driver::CharDevDescriptor,
    error::Result,
    fs::{OpenFlags, attr::FilePermissions},
    memory::address::UA,
};

struct RandomFileOps;

#[async_trait]
impl FileOps for RandomFileOps {
    async fn read(&mut self, _ctx: &mut FileCtx, buf: UA, count: usize) -> Result<usize> {
        self.readat(buf, count, 0).await
    }

    async fn writeat(&mut self, _buf: UA, count: usize, _offset: u64) -> Result<usize> {
        // Just consume the write.
        Ok(count)
    }

    async fn readat(&mut self, buf: UA, count: usize, _offset: u64) -> Result<usize> {
        sys_getrandom(buf.cast(), count as _, 0).await
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        Box::pin(async { Ok(()) })
    }
}

struct RandomDev;

impl OpenableDevice for RandomDev {
    fn open(&self, flags: OpenFlags) -> Result<Arc<OpenFile>> {
        Ok(Arc::new(OpenFile::new(Box::new(RandomFileOps), flags)))
    }
}

struct RandomCharDev {
    random_dev: Arc<dyn OpenableDevice>,
}

impl RandomCharDev {
    fn new() -> Result<Self> {
        devfs().mknod(
            "random".to_string(),
            CharDevDescriptor {
                major: ReservedMajors::Random as _,
                minor: 0,
            },
            FilePermissions::from_bits_retain(0o666),
        )?;

        Ok(Self {
            random_dev: Arc::new(RandomDev),
        })
    }
}

impl CharDriver for RandomCharDev {
    fn get_device(&self, minor: u64) -> Option<Arc<dyn OpenableDevice>> {
        if minor == 0 {
            Some(self.random_dev.clone())
        } else {
            None
        }
    }
}

/// Driver initialisation entry point invoked during kernel boot.
pub fn random_chardev_init(_bus: &mut PlatformBus, dm: &mut DriverManager) -> Result<()> {
    let cdev = RandomCharDev::new()?;
    dm.register_char_driver(ReservedMajors::Random as _, Arc::new(cdev))
}

kernel_driver!(random_chardev_init);
