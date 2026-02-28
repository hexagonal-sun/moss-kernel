use crate::drivers::fs::dev::devfs;
use crate::drivers::init::PlatformBus;
use crate::drivers::{CharDriver, DriverManager, OpenableDevice, ReservedMajors};
use crate::fs::fops::FileOps;
use crate::fs::open_file::{FileCtx, OpenFile};
use crate::kernel_driver;
use crate::memory::uaccess::{copy_from_user_slice, copy_to_user_slice};
use crate::sync::OnceLock;
use alloc::string::ToString;
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use core::pin::Pin;
use libkernel::driver::CharDevDescriptor;
use libkernel::error::KernelError;
use libkernel::fs::OpenFlags;
use libkernel::fs::attr::FilePermissions;
use libkernel::memory::address::UA;

pub mod virtio;

/// Minimal kernel-facing display abstraction.
///
/// This is intentionally small: it provides a framebuffer (RGBA8888) and the
/// ability to flush updated contents to the device.
///
/// The underlying implementation may be MMIO, VirtIO, etc.
pub trait Display: Send + Sync + super::Driver {
    /// Returns the current resolution in pixels.
    fn resolution(&self) -> (usize, usize);

    /// Locks and returns a mutable view of the framebuffer in RGBA8888 format.
    ///
    /// The returned guard must keep the lock held for as long as the slice is used,
    /// preventing unsound aliasing.
    ///
    /// Length must be `width * height * 4`.
    fn lock_framebuffer(&self) -> FramebufferGuard<'_>;

    /// Flush any pending framebuffer updates to the physical display.
    fn flush(&self) -> libkernel::error::Result<()>;
}

/// A guard that keeps the underlying display locked while the framebuffer slice is borrowed.
///
/// Internally, this stores a small drop-closure that owns the lock guard. When the
/// `FramebufferGuard` is dropped, the closure is dropped, releasing the lock guard.
pub struct FramebufferGuard<'a> {
    fb: &'a mut [u8],
    _drop_guard: Box<dyn FnOnce() + 'a>,
}

impl<'a> FramebufferGuard<'a> {
    /// Creates a framebuffer guard from an existing lock guard plus a framebuffer slice.
    ///
    /// This is primarily intended for driver implementations.
    pub fn new<G>(guard: G, fb: &'a mut [u8]) -> Self
    where
        G: 'a,
    {
        Self {
            fb,
            _drop_guard: Box::new(move || drop(guard)),
        }
    }

    #[expect(unused)]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.fb
    }
}

impl<'a> core::ops::Deref for FramebufferGuard<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.fb
    }
}

impl<'a> core::ops::DerefMut for FramebufferGuard<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.fb
    }
}

unsafe impl<'a> Send for FramebufferGuard<'a> {}
unsafe impl<'a> Sync for FramebufferGuard<'a> {}

/// Global system display, if any.
///
/// A platform may choose to only expose a single display device.
static SYS_DISPLAY: OnceLock<Arc<dyn Display>> = OnceLock::new();

/// Sets the system display instance. Returns `Err(display)` if one is already set.
pub fn set_system_display(display: Arc<dyn Display>) -> Result<(), Arc<dyn Display>> {
    SYS_DISPLAY.set(display)
}

/// Returns the system display, if initialised.
pub fn system_display() -> Option<Arc<dyn Display>> {
    SYS_DISPLAY.get().cloned()
}

/// `/dev/fb0` file operations.
struct FbFileOps;

#[async_trait]
impl FileOps for FbFileOps {
    async fn readat(
        &mut self,
        buf: UA,
        count: usize,
        offset: u64,
    ) -> libkernel::error::Result<usize> {
        let display = system_display().ok_or(KernelError::Other("no display device"))?;
        let (width, height) = display.resolution();
        let fb_size = width * height * 4;
        let amount_to_read = core::cmp::min(count, fb_size.saturating_sub(offset as usize));
        if amount_to_read == 0 {
            return Ok(0);
        }
        let fb_guard = display.lock_framebuffer();
        let fb_slice = &fb_guard[offset as usize..offset as usize + amount_to_read];
        copy_to_user_slice(fb_slice, buf).await?;
        Ok(amount_to_read)
    }

    async fn writeat(
        &mut self,
        buf: UA,
        count: usize,
        offset: u64,
    ) -> libkernel::error::Result<usize> {
        let display = system_display().ok_or(KernelError::Other("no display device"))?;
        let (width, height) = display.resolution();
        let fb_size = width * height * 4;
        let amount_to_write = core::cmp::min(count, fb_size.saturating_sub(offset as usize));
        if amount_to_write == 0 {
            return Ok(0);
        }
        let mut fb_guard = display.lock_framebuffer();
        let fb_slice = &mut fb_guard[offset as usize..offset as usize + amount_to_write];
        copy_from_user_slice(buf, fb_slice).await?;
        drop(fb_guard); // Explicitly drop the guard before flushing, to release the lock.
        display.flush()?;
        Ok(amount_to_write)
    }

    fn poll_read_ready(
        &self,
    ) -> Pin<Box<dyn Future<Output = libkernel::error::Result<()>> + Send>> {
        // Always ready to read (but will return EOF).
        Box::pin(async { Ok(()) })
    }

    fn poll_write_ready(
        &self,
    ) -> Pin<Box<dyn Future<Output = libkernel::error::Result<()>> + Send>> {
        // Always ready to accept writes.
        Box::pin(async { Ok(()) })
    }

    async fn ioctl(
        &mut self,
        _ctx: &mut FileCtx,
        request: usize,
        _argp: usize,
    ) -> libkernel::error::Result<usize> {
        const FBIOGET_VSCREENINFO: usize = 0x4600;
        const FBIOPUT_VSCREENINFO: usize = 0x4601;
        const FBIOGET_FSCREENINFO: usize = 0x4602;
        const FBIOPAN_DISPLAY: usize = 0x4606;

        match request {
            FBIOGET_VSCREENINFO => todo!(),
            FBIOPUT_VSCREENINFO => todo!(),
            FBIOGET_FSCREENINFO => todo!(),
            FBIOPAN_DISPLAY => todo!(),
            _ => Err(KernelError::InvalidValue),
        }
    }
}

struct FbDev;

impl OpenableDevice for FbDev {
    fn open(&self, flags: OpenFlags) -> libkernel::error::Result<Arc<OpenFile>> {
        Ok(Arc::new(OpenFile::new(Box::new(FbFileOps), flags)))
    }
}

struct FbCharDev {
    fb_dev: Arc<dyn OpenableDevice>,
}

impl FbCharDev {
    fn new() -> libkernel::error::Result<Self> {
        // Register the /dev/fb0 node in devfs.
        devfs().mknod(
            "fb0".to_string(),
            CharDevDescriptor {
                major: ReservedMajors::Fb as _,
                minor: 0,
            },
            // World-writable, world-readable like on Linux.
            FilePermissions::from_bits_retain(0o666),
        )?;

        Ok(Self {
            fb_dev: Arc::new(FbDev),
        })
    }
}

impl CharDriver for FbCharDev {
    fn get_device(&self, minor: u64) -> Option<Arc<dyn OpenableDevice>> {
        if minor == 0 {
            Some(self.fb_dev.clone())
        } else {
            None
        }
    }
}

pub fn fb0_chardev_init(
    _bus: &mut PlatformBus,
    dm: &mut DriverManager,
) -> libkernel::error::Result<()> {
    let cdev = FbCharDev::new()?;
    dm.register_char_driver(ReservedMajors::Fb as _, Arc::new(cdev))
}

kernel_driver!(fb0_chardev_init);
