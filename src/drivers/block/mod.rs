use alloc::{boxed::Box, sync::Arc, vec::Vec};
use async_trait::async_trait;
use libkernel::{error::Result, fs::BlockDevice};
use log::info;

use crate::sync::SpinLock;

pub mod virtio;

struct RegisteredBlockDevice {
    name: &'static str,
    device: Arc<dyn BlockDevice>,
}

struct SharedBlockDevice {
    inner: Arc<dyn BlockDevice>,
}

#[async_trait]
impl BlockDevice for SharedBlockDevice {
    async fn read(&self, block_id: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read(block_id, buf).await
    }

    async fn write(&self, block_id: u64, buf: &[u8]) -> Result<()> {
        self.inner.write(block_id, buf).await
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    async fn sync(&self) -> Result<()> {
        self.inner.sync().await
    }
}

static BLOCK_DEVICES: SpinLock<Vec<RegisteredBlockDevice>> = SpinLock::new(Vec::new());

pub fn register_block_device(name: &'static str, device: Arc<dyn BlockDevice>) -> usize {
    let mut devices = BLOCK_DEVICES.lock_save_irq();
    let index = devices.len();

    devices.push(RegisteredBlockDevice { name, device });
    info!("registered block device {name} as index {index}");

    index
}

pub fn get_block_device(index: usize) -> Option<(&'static str, Box<dyn BlockDevice>)> {
    let devices = BLOCK_DEVICES.lock_save_irq();
    let device = devices.get(index)?;

    Some((
        device.name,
        Box::new(SharedBlockDevice {
            inner: device.device.clone(),
        }),
    ))
}

pub fn first_block_device() -> Option<(&'static str, Box<dyn BlockDevice>)> {
    get_block_device(0)
}
