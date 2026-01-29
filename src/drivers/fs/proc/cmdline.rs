use crate::arch::{Arch, ArchImpl};
use alloc::boxed::Box;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::fs::attr::FileAttr;
use libkernel::fs::{InodeId, SimpleFile};

pub struct ProcCmdlineInode {
    id: InodeId,
    attr: FileAttr,
}

impl ProcCmdlineInode {
    pub fn new(id: InodeId) -> Self {
        Self {
            id,
            attr: FileAttr {
                file_type: libkernel::fs::FileType::File,
                mode: libkernel::fs::attr::FilePermissions::from_bits_retain(0o444),
                ..FileAttr::default()
            },
        }
    }
}

#[async_trait]
impl SimpleFile for ProcCmdlineInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> libkernel::error::Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn read(&self) -> libkernel::error::Result<Vec<u8>> {
        let cmdline = ArchImpl::get_cmdline().unwrap_or_default();
        Ok(cmdline.into_bytes())
    }
}
