use crate::drivers::virtio_hal::VirtioHal;
use crate::sync::SpinLock;
use crate::{
    arch::ArchImpl,
    drivers::{
        Driver, DriverManager,
        display::{Display, FramebufferGuard, set_system_display},
        init::PlatformBus,
        probe::{DeviceDescriptor, DeviceMatchType},
    },
    kernel_driver,
};
use alloc::{boxed::Box, sync::Arc};
use core::ptr::NonNull;
use libkernel::{
    KernAddressSpace, VirtualMemory,
    error::{KernelError, ProbeError, Result},
    memory::{
        address::{PA, VA},
        region::PhysMemoryRegion,
    },
};
use log::{info, warn};
use virtio_drivers::{
    device::gpu::VirtIOGpu,
    transport::{
        DeviceType, Transport,
        mmio::{MmioTransport, VirtIOHeader},
    },
};

/// A system display backed by VirtIO-GPU in framebuffer mode.
pub struct VirtioGpuDisplay<T: Transport + Send> {
    fdt_name: Option<&'static str>,
    gpu: SpinLock<VirtIOGpu<VirtioHal, T>>,
    fb_addr: SpinLock<usize>,
    fb_len: SpinLock<usize>,
}

impl<T: Transport + Send> VirtioGpuDisplay<T> {
    fn new(fdt_name: Option<&'static str>, transport: T) -> Result<Self> {
        let mut gpu = VirtIOGpu::<VirtioHal, T>::new(transport)
            .map_err(|_| KernelError::Other("virtio-gpu init failed"))?;

        let (width, height) = gpu.resolution().expect("failed to get resolution");
        let width = width as usize;
        let height = height as usize;
        info!("GPU resolution is {width}x{height}");

        // Allocate/setup framebuffer once; keep it alive for driver lifetime.
        let fb: &mut [u8] = gpu
            .setup_framebuffer()
            .map_err(|_| KernelError::Other("virtio-gpu framebuffer setup failed"))?;

        let fb_addr = fb.as_mut_ptr() as usize;
        let fb_len = fb.len();

        Ok(Self {
            fdt_name,
            gpu: crate::sync::SpinLock::new(gpu),
            fb_addr: crate::sync::SpinLock::new(fb_addr),
            fb_len: crate::sync::SpinLock::new(fb_len),
        })
    }
}

impl<T: Transport + Send + Sync + 'static> Driver for VirtioGpuDisplay<T> {
    fn name(&self) -> &'static str {
        self.fdt_name.unwrap_or("virtio-gpu")
    }
}

impl<T: Transport + Send + Sync + 'static> Display for VirtioGpuDisplay<T> {
    fn resolution(&self) -> (usize, usize) {
        let mut gpu = self.gpu.lock_save_irq();
        match gpu.resolution() {
            Ok((w, h)) => (w as usize, h as usize),
            Err(_) => (0, 0),
        }
    }

    fn lock_framebuffer(&self) -> FramebufferGuard<'_> {
        let gpu_guard = self.gpu.lock_save_irq();

        // Lock ordering: take the GPU lock first, then borrow the framebuffer address/len.
        // These are stable for the driver lifetime (set once in `new()`).
        let fb_addr = *self.fb_addr.lock_save_irq();
        let fb_len = *self.fb_len.lock_save_irq();

        // SAFETY: The framebuffer was created by `setup_framebuffer()` during init and is owned
        // by `VirtIOGpu` inside `gpu_guard`. Holding `gpu_guard` ensures exclusive access.
        let fb: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(fb_addr as *mut u8, fb_len) };

        FramebufferGuard::new(gpu_guard, fb)
    }

    fn flush(&self) -> Result<()> {
        let mut gpu = self.gpu.lock_save_irq();
        gpu.flush()
            .map_err(|_| KernelError::Other("virtio-gpu flush failed"))
    }
}

fn virtio_gpu_probe(_dm: &mut DriverManager, d: DeviceDescriptor) -> Result<Arc<dyn Driver>> {
    match d {
        DeviceDescriptor::Fdt(fdt_node, _flags) => {
            // VirtIO-MMIO devices are described by a single reg region containing the header.
            let region = fdt_node
                .reg()
                .ok_or(ProbeError::NoReg)?
                .next()
                .ok_or(ProbeError::NoReg)?;

            let size = region.size.ok_or(ProbeError::NoRegSize)?;

            let mapped: VA =
                ArchImpl::kern_address_space()
                    .lock_save_irq()
                    .map_mmio(PhysMemoryRegion::new(
                        PA::from_value(region.address as usize),
                        size,
                    ))?;

            // SAFETY: mapped points to a valid VirtIO MMIO header for the device.
            let header = NonNull::new(mapped.value() as *mut VirtIOHeader)
                .ok_or(KernelError::InvalidValue)?;

            // Construct the transport first; it can report its device type.
            let transport = unsafe {
                MmioTransport::new(header, size).map_err(|e| {
                    log::error!("Failed to initialize virtio-mmio transport: {e}");
                    KernelError::Other("virtio-mmio transport init failed")
                })?
            };

            // Only bind to GPU here; other virtio-mmio devices should be handled by their own drivers.
            if !matches!(transport.device_type(), DeviceType::GPU) {
                return Err(KernelError::Probe(ProbeError::Deferred));
            }

            if mapped != VA::from_value(0xffffd00000f92e00) {
                return Err(KernelError::Probe(ProbeError::Deferred));
            }
            info!("virtio-gpu found at {mapped:?} (node {})", fdt_node.name);

            let disp = Arc::new(VirtioGpuDisplay::new(Some(fdt_node.name), transport)?);

            // Register as system display if none exists.
            if set_system_display(disp.clone()).is_err() {
                warn!("System display already set; virtio-gpu will not be used as primary");
            } else {
                let (w, h) = disp.resolution();
                info!("System display set to virtio-gpu ({w}x{h})");
            }

            Ok(disp)
        }
    }
}

pub fn virtio_gpu_init(bus: &mut PlatformBus, _dm: &mut DriverManager) -> Result<()> {
    // Common compatible strings for virtio-mmio on DT.
    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("virtio,mmio"),
        Box::new(virtio_gpu_probe),
    );

    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("virtio-mmio"),
        Box::new(virtio_gpu_probe),
    );

    Ok(())
}

kernel_driver!(virtio_gpu_init);
