use crate::{drivers::Driver, fs::FilesystemDriver};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use libkernel::{
    error::{KernelError, Result},
    fs::{BlockDevice, Filesystem, blk::buffer::BlockBuffer, filesystems::ext4::Ext4Filesystem},
};
use log::warn;

pub struct Ext4FsDriver {}

impl Ext4FsDriver {
    pub fn new() -> Self {
        Self {}
    }
}

impl Driver for Ext4FsDriver {
    fn name(&self) -> &'static str {
        "ext4fs"
    }

    fn as_filesystem_driver(self: Arc<Self>) -> Option<Arc<dyn FilesystemDriver>> {
        Some(self)
    }
}

#[async_trait]
impl FilesystemDriver for Ext4FsDriver {
    async fn construct(
        &self,
        fs_id: u64,
        device: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        match device {
            Some(dev) => Ok(Ext4Filesystem::new(BlockBuffer::new(dev), fs_id).await?),
            None => {
                warn!("Could not mount fat32 fs with no block device");
                Err(KernelError::InvalidValue)
            }
        }
    }
}
