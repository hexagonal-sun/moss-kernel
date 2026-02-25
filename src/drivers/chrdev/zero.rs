use crate::{
    drivers::{
        CharDriver, DriverManager, OpenableDevice, ReservedMajors, fs::dev::devfs,
        init::PlatformBus,
    },
    fs::{fops::FileOps, open_file::FileCtx, open_file::OpenFile},
    kernel_driver,
    memory::uaccess::copy_to_user_slice,
};
use alloc::{boxed::Box, string::ToString, sync::Arc};
use async_trait::async_trait;
use core::{cmp::min, future::Future, pin::Pin};
use libkernel::{
    driver::CharDevDescriptor,
    error::Result,
    fs::{OpenFlags, attr::FilePermissions},
    memory::address::UA,
};

const USER_COPY_CHUNK_SIZE: usize = 0x100;

static ZERO_BUF: [u8; USER_COPY_CHUNK_SIZE] = [0u8; USER_COPY_CHUNK_SIZE];

struct ZeroFileOps;

#[async_trait]
impl FileOps for ZeroFileOps {
    async fn read(&mut self, _ctx: &mut FileCtx, buf: UA, count: usize) -> Result<usize> {
        self.readat(buf, count, 0).await
    }

    async fn readat(&mut self, mut buf: UA, mut count: usize, _offset: u64) -> Result<usize> {
        let requested = count;

        while count > 0 {
            let chunk_sz = min(count, USER_COPY_CHUNK_SIZE);
            copy_to_user_slice(&ZERO_BUF[..chunk_sz], buf).await?;

            buf = buf.add_bytes(chunk_sz);
            count -= chunk_sz;
        }

        Ok(requested)
    }

    async fn write(&mut self, _ctx: &mut FileCtx, buf: UA, count: usize) -> Result<usize> {
        self.writeat(buf, count, 0).await
    }

    async fn writeat(&mut self, _buf: UA, count: usize, _offset: u64) -> Result<usize> {
        Ok(count)
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        Box::pin(async { Ok(()) })
    }

    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        Box::pin(async { Ok(()) })
    }
}

struct ZeroDev;

impl OpenableDevice for ZeroDev {
    fn open(&self, flags: OpenFlags) -> Result<Arc<OpenFile>> {
        Ok(Arc::new(OpenFile::new(Box::new(ZeroFileOps), flags)))
    }
}

struct ZeroCharDev {
    zero_dev: Arc<dyn OpenableDevice>,
}

impl ZeroCharDev {
    fn new() -> Result<Self> {
        devfs().mknod(
            "zero".to_string(),
            CharDevDescriptor {
                major: ReservedMajors::Zero as _,
                minor: 0,
            },
            FilePermissions::from_bits_retain(0o666),
        )?;

        Ok(Self {
            zero_dev: Arc::new(ZeroDev),
        })
    }
}

impl CharDriver for ZeroCharDev {
    fn get_device(&self, minor: u64) -> Option<Arc<dyn OpenableDevice>> {
        if minor == 0 {
            Some(self.zero_dev.clone())
        } else {
            None
        }
    }
}

/// Driver initialisation entry point invoked during kernel boot.
pub fn zero_chardev_init(_bus: &mut PlatformBus, dm: &mut DriverManager) -> Result<()> {
    let cdev = ZeroCharDev::new()?;
    dm.register_char_driver(ReservedMajors::Zero as _, Arc::new(cdev))
}

kernel_driver!(zero_chardev_init);
