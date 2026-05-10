use crate::drivers::virtio_hal::VirtioHal;
use crate::sync::SpinLock;
use crate::{
    arch::ArchImpl,
    drivers::{
        Driver, DriverManager,
        init::PlatformBus,
        probe::{DeviceDescriptor, DeviceMatchType},
    },
    kernel_driver,
};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use core::ptr::NonNull;
use libkernel::memory::proc_vm::address_space::{KernAddressSpace, VirtualMemory};
use libkernel::{
    error::{IoError, KernelError, ProbeError, Result},
    fs::BlockDevice,
    memory::{
        address::{PA, VA},
        region::PhysMemoryRegion,
    },
};
use log::info;
use virtio_drivers::{
    Error as VirtioError,
    device::blk::{SECTOR_SIZE, VirtIOBlk},
    transport::{
        DeviceType, Transport,
        mmio::{MmioTransport, VirtIOHeader},
    },
};

pub struct VirtioBlkDriver<T: Transport + Send> {
    fdt_name: Option<&'static str>,
    blk: SpinLock<VirtIOBlk<VirtioHal, T>>,
    capacity_sectors: u64,
    readonly: bool,
}

impl<T: Transport + Send> VirtioBlkDriver<T> {
    pub fn new(fdt_name: Option<&'static str>, transport: T) -> Result<Self> {
        let blk = VirtIOBlk::<VirtioHal, T>::new(transport)
            .map_err(|_| KernelError::Other("virtio-blk init failed"))?;

        let capacity_sectors = blk.capacity();
        let readonly = blk.readonly();

        info!(
            "virtio-blk capacity={} sectors ({} bytes), readonly={readonly}",
            capacity_sectors,
            capacity_sectors * SECTOR_SIZE as u64,
        );

        Ok(Self {
            fdt_name,
            blk: SpinLock::new(blk),
            capacity_sectors,
            readonly,
        })
    }

    fn validate_io(&self, block_id: u64, len: usize) -> Result<usize> {
        if len == 0 {
            return usize::try_from(block_id).map_err(|_| KernelError::RangeError);
        }

        if !len.is_multiple_of(SECTOR_SIZE) {
            return Err(KernelError::InvalidValue);
        }

        let sectors = (len / SECTOR_SIZE) as u64;
        let end = block_id.checked_add(sectors).ok_or(IoError::OutOfBounds)?;
        if end > self.capacity_sectors {
            return Err(IoError::OutOfBounds.into());
        }

        usize::try_from(block_id).map_err(|_| KernelError::RangeError)
    }
}

impl<T: Transport + Send + Sync + 'static> Driver for VirtioBlkDriver<T> {
    fn name(&self) -> &'static str {
        self.fdt_name.unwrap_or("virtio-blk")
    }

    fn as_block_device(self: Arc<Self>) -> Option<Arc<dyn BlockDevice>> {
        Some(self)
    }
}

#[async_trait]
impl<T: Transport + Send + Sync> BlockDevice for VirtioBlkDriver<T> {
    async fn read(&self, block_id: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }

        let block_id = self.validate_io(block_id, buf.len())?;
        let mut blk = self.blk.lock_save_irq();

        blk.read_blocks(block_id, buf).map_err(map_virtio_error)
    }

    async fn write(&self, block_id: u64, buf: &[u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }

        if self.readonly {
            return Err(KernelError::NotPermitted);
        }

        let block_id = self.validate_io(block_id, buf.len())?;
        let mut blk = self.blk.lock_save_irq();

        blk.write_blocks(block_id, buf).map_err(map_virtio_error)
    }

    fn block_size(&self) -> usize {
        SECTOR_SIZE
    }

    async fn sync(&self) -> Result<()> {
        let mut blk = self.blk.lock_save_irq();
        blk.flush().map_err(map_virtio_error)
    }
}

fn map_virtio_error(error: VirtioError) -> KernelError {
    match error {
        VirtioError::QueueFull => KernelError::BufferFull,
        VirtioError::NotReady => KernelError::Other("virtio-blk device not ready"),
        VirtioError::WrongToken => KernelError::Other("virtio-blk queue token mismatch"),
        VirtioError::AlreadyUsed => KernelError::InUse,
        VirtioError::InvalidParam => KernelError::InvalidValue,
        VirtioError::DmaError => KernelError::NoMemory,
        VirtioError::IoError => KernelError::Other("virtio-blk I/O error"),
        VirtioError::Unsupported => KernelError::NotSupported,
        VirtioError::ConfigSpaceTooSmall => KernelError::Other("virtio-blk config space too small"),
        VirtioError::ConfigSpaceMissing => KernelError::Other("virtio-blk config space missing"),
        VirtioError::SocketDeviceError(_) => KernelError::Other("virtio-blk transport error"),
    }
}

fn virtio_blk_probe(_dm: &mut DriverManager, d: DeviceDescriptor) -> Result<Arc<dyn Driver>> {
    match d {
        DeviceDescriptor::Fdt(fdt_node, _flags) => {
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

            let header = NonNull::new(mapped.value() as *mut VirtIOHeader)
                .ok_or(KernelError::InvalidValue)?;

            let transport = unsafe {
                match MmioTransport::new(header, size) {
                    Ok(t) => t,
                    Err(_) => return Err(KernelError::Probe(ProbeError::NoMatch)),
                }
            };

            if !matches!(transport.device_type(), DeviceType::Block) {
                return Err(KernelError::Probe(ProbeError::NoMatch));
            }

            info!("virtio-blk found at {mapped:?} (node {})", fdt_node.name);

            Ok(Arc::new(VirtioBlkDriver::new(
                Some(fdt_node.name),
                transport,
            )?))
        }
    }
}

pub fn virtio_blk_init(bus: &mut PlatformBus, _dm: &mut DriverManager) -> Result<()> {
    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("virtio,mmio"),
        Box::new(virtio_blk_probe),
    );

    bus.register_platform_driver(
        DeviceMatchType::FdtCompatible("virtio-mmio"),
        Box::new(virtio_blk_probe),
    );

    Ok(())
}

kernel_driver!(virtio_blk_init);
