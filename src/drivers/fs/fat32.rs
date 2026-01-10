use crate::arch::ArchImpl;
use crate::{drivers::Driver, fs::FilesystemDriver};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use libkernel::{
    error::{KernelError, Result},
    fs::{BlockDevice, Filesystem, blk::buffer::BlockBuffer, filesystems::fat32::Fat32Filesystem},
};
use log::warn;

pub struct Fat32FsDriver {}

impl Fat32FsDriver {
    pub fn new() -> Self {
        Self {}
    }
}

impl Driver for Fat32FsDriver {
    fn name(&self) -> &'static str {
        "fat32fs"
    }

    fn as_filesystem_driver(self: Arc<Self>) -> Option<Arc<dyn FilesystemDriver>> {
        Some(self)
    }
}

#[async_trait]
impl FilesystemDriver for Fat32FsDriver {
    async fn construct(
        &self,
        fs_id: u64,
        device: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        match device {
            Some(dev) => Ok(Fat32Filesystem::new(BlockBuffer::<ArchImpl>::new(dev), fs_id).await?),
            None => {
                warn!("Could not mount fat32 fs with no block device");
                Err(KernelError::InvalidValue)
            }
        }
    }
}
