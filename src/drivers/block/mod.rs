use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use async_trait::async_trait;
use libkernel::{
    driver::CharDevDescriptor,
    error::Result,
    fs::{BlockDevice, attr::FilePermissions},
};
use log::info;

use crate::{drivers::fs::dev::devfs, sync::SpinLock};

pub mod virtio;

pub const BLOCK_DEVICE_MAJOR: u64 = 254;

struct RegisteredBlockDevice {
    name: &'static str,
    descriptor: CharDevDescriptor,
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

fn block_device_devfs_name(index: usize) -> String {
    let mut suffix = String::new();
    let mut value = index;

    loop {
        let c = (b'a' + (value % 26) as u8) as char;
        suffix.insert(0, c);

        if value < 26 {
            break;
        }

        value = value / 26 - 1;
    }

    format!("vd{suffix}")
}

pub fn register_block_device(name: &'static str, device: Arc<dyn BlockDevice>) -> usize {
    let mut devices = BLOCK_DEVICES.lock_save_irq();
    let index = devices.len();
    let descriptor = CharDevDescriptor {
        major: BLOCK_DEVICE_MAJOR,
        minor: index as u64,
    };
    let devfs_name = block_device_devfs_name(index);
    let block_size = device.block_size() as u32;

    devfs()
        .mknod_block(
            devfs_name.clone(),
            descriptor,
            FilePermissions::from_bits_retain(0o660),
            block_size,
        )
        .expect("newly-allocated block device name should be unique");

    devices.push(RegisteredBlockDevice {
        name,
        descriptor,
        device,
    });
    info!("registered block device {name} as index {index} at /dev/{devfs_name} ({descriptor:?})");

    index
}

#[expect(unused)]
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

pub fn get_block_device_by_descriptor(
    descriptor: CharDevDescriptor,
) -> Option<(&'static str, Box<dyn BlockDevice>)> {
    let devices = BLOCK_DEVICES.lock_save_irq();
    let device = devices
        .iter()
        .find(|device| device.descriptor == descriptor)?;

    Some((
        device.name,
        Box::new(SharedBlockDevice {
            inner: device.device.clone(),
        }),
    ))
}

#[expect(unused)]
pub fn first_block_device() -> Option<(&'static str, Box<dyn BlockDevice>)> {
    get_block_device(0)
}
