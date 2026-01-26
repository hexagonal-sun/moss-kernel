use crate::memory::PAGE_ALLOC;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::fs::attr::FileAttr;
use libkernel::fs::{InodeId, SimpleFile};
use libkernel::memory::PAGE_SIZE;

pub struct ProcMeminfoInode {
    id: InodeId,
    attr: FileAttr,
}

impl ProcMeminfoInode {
    pub fn new(inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: libkernel::fs::FileType::File,
                ..FileAttr::default()
            },
        }
    }
}

#[async_trait]
impl SimpleFile for ProcMeminfoInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> libkernel::error::Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn read(&self) -> libkernel::error::Result<Vec<u8>> {
        // Gather memory statistics from the global page allocator.
        let page_alloc = PAGE_ALLOC.get().expect("PAGE_ALLOC must be initialised");

        let total_pages = page_alloc.total_pages();
        let free_pages = page_alloc.free_pages();

        let total_ram = (total_pages * PAGE_SIZE) / 1024;
        let free_ram = (free_pages * PAGE_SIZE) / 1204;
        let mut meminfo_content = String::new();
        meminfo_content.push_str(&format!("MemTotal: {total_ram} kB\n"));
        meminfo_content.push_str(&format!("MemFree: {free_ram} kB\n"));
        Ok(meminfo_content.into_bytes())
    }
}
