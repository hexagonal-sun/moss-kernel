use crate::{
    CpuOps,
    error::{FsError, Result},
    fs::{DirStream, Dirent, FileType, Filesystem, Inode, InodeId, attr::FileAttr},
    sync::spinlock::SpinLockIrq,
};
use alloc::{
    boxed::Box,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use async_trait::async_trait;

struct TmpFsDirEnt {
    name: String,
    id: InodeId,
    kind: FileType,
    inode: Arc<dyn Inode>,
}

struct TmpFsDirInode<CPU: CpuOps> {
    entries: SpinLockIrq<Vec<TmpFsDirEnt>, CPU>,
    attrs: FileAttr,
    id: u64,
    fs: Weak<TmpFs<CPU>>,
    this: Weak<Self>,
}

struct TmpFsDirReader<CPU: CpuOps> {
    inode: Arc<TmpFsDirInode<CPU>>,
    offset: usize,
}

#[async_trait]
impl<CPU: CpuOps> DirStream for TmpFsDirReader<CPU> {
    async fn next_entry(&mut self) -> Result<Option<Dirent>> {
        if let Some(entry) = self.inode.entries.lock_save_irq().get(self.offset) {
            let dent = Some(Dirent {
                id: entry.id,
                name: entry.name.clone(),
                file_type: entry.kind,
                offset: self.offset as _,
            });

            self.offset += 1;

            Ok(dent)
        } else {
            Ok(None)
        }
    }
}

#[async_trait]
impl<CPU: CpuOps> Inode for TmpFsDirInode<CPU> {
    fn id(&self) -> crate::fs::InodeId {
        InodeId::from_fsid_and_inodeid(self.fs.upgrade().unwrap().id(), self.id)
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        self.entries
            .lock_save_irq()
            .iter()
            .find(|x| x.name == name)
            .map(|x| x.inode.clone())
            .ok_or(FsError::NotFound.into())
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attrs.clone())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        Ok(Box::new(TmpFsDirReader {
            inode: self.this.upgrade().unwrap(),
            offset: start_offset as _,
        }))
    }
}

pub struct TmpFs<CPU: CpuOps> {
    id: u64,
    next_inode_id: u64,
    this: Weak<Self>,
    root: Arc<TmpFsDirInode<CPU>>,
}

#[async_trait]
impl<CPU: CpuOps> Filesystem for TmpFs<CPU> {
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Ok(self.root.clone())
    }

    fn id(&self) -> u64 {
        self.id
    }
}
