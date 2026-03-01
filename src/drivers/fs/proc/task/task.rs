use crate::drivers::fs::proc::task::ProcTaskInode;
use crate::drivers::fs::proc::{get_inode_id, procfs};
use crate::process::thread_group::Tgid;
use crate::process::{TaskDescriptor, Tid, find_process_by_tgid, find_task_by_descriptor};
use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::error::FsError;
use libkernel::fs::attr::FileAttr;
use libkernel::fs::{DirStream, Dirent, FileType, Filesystem, Inode, InodeId, SimpleDirStream};

pub struct ProcTaskDirInode {
    id: InodeId,
    attr: FileAttr,
    tgid: Tgid,
}

impl ProcTaskDirInode {
    pub fn new(tgid: Tgid, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: FileType::Directory,
                // Define appropriate file attributes for fdinfo.
                ..FileAttr::default()
            },
            tgid,
        }
    }
}

#[async_trait]
impl Inode for ProcTaskDirInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> libkernel::error::Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn lookup(&self, name: &str) -> libkernel::error::Result<Arc<dyn Inode>> {
        let tid = match name.parse::<u32>() {
            Ok(tid) => Tid(tid),
            Err(_) => return Err(FsError::NotFound.into()),
        };
        let fs = procfs();
        let inode_id = InodeId::from_fsid_and_inodeid(
            fs.id(),
            get_inode_id(&[&self.tgid.value().to_string(), &tid.value().to_string()]),
        );
        let desc = TaskDescriptor::from_tgid_tid(self.tgid, tid);
        find_task_by_descriptor(&desc).ok_or(FsError::NotFound)?;
        Ok(Arc::new(ProcTaskInode::new(desc, true, inode_id)))
    }

    async fn readdir(&self, start_offset: u64) -> libkernel::error::Result<Box<dyn DirStream>> {
        let process = &find_process_by_tgid(self.tgid)
            .ok_or(FsError::NotFound)?
            .process;
        let tasks = process.tasks.lock_save_irq();
        let mut entries = Vec::new();
        for (i, (_tid, task)) in tasks.iter().enumerate().skip(start_offset as usize) {
            let Some(task) = task.upgrade() else {
                continue;
            };
            let id = InodeId::from_fsid_and_inodeid(
                procfs().id(),
                get_inode_id(&[
                    &self.tgid.value().to_string(),
                    &task.tid.value().to_string(),
                ]),
            );
            entries.push(Dirent {
                id,
                offset: (i + 1) as u64,
                file_type: FileType::Directory,
                name: task.tid.value().to_string(),
            });
        }
        Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
    }
}
