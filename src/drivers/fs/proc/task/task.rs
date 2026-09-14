use crate::drivers::fs::proc::get_inode_id;
use crate::drivers::fs::proc::task::ProcTaskInode;
use crate::process::{Tid, find_task_by_tid};
use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::error::FsError;
use libkernel::fs::attr::FileAttr;
use libkernel::fs::{DirStream, Dirent, FileType, Inode, InodeId, SimpleDirStream};

pub struct ProcTaskDirInode {
    id: InodeId,
    attr: FileAttr,
    tid: Tid,
    ns: Arc<crate::process::pid_namespace::PidNamespace>,
}

impl ProcTaskDirInode {
    pub fn new(
        tid: Tid,
        ns: Arc<crate::process::pid_namespace::PidNamespace>,
        inode_id: InodeId,
    ) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: FileType::Directory,
                // Define appropriate file attributes for fdinfo.
                ..FileAttr::default()
            },
            tid,
            ns,
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
            Ok(tid) => self.ns.resolve(tid).ok_or(FsError::NotFound)?,
            Err(_) => return Err(FsError::NotFound.into()),
        };
        let inode_id = InodeId::from_fsid_and_inodeid(
            self.id.fs_id(),
            get_inode_id(&[&self.tid.value().to_string(), &tid.value().to_string()]),
        );
        let parent = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
        let target = find_task_by_tid(tid).ok_or(FsError::NotFound)?;
        if parent.process.tgid != target.process.tgid {
            return Err(FsError::NotFound.into());
        }
        Ok(Arc::new(ProcTaskInode::new(
            tid,
            true,
            self.ns.clone(),
            inode_id,
        )))
    }

    async fn readdir(&self, start_offset: u64) -> libkernel::error::Result<Box<dyn DirStream>> {
        let process = &find_task_by_tid(self.tid).ok_or(FsError::NotFound)?.process;
        let tasks = process.tasks.lock_save_irq();
        let mut entries = Vec::new();
        for (i, (_tid, task)) in tasks.iter().enumerate() {
            let Some(task) = task.upgrade() else {
                continue;
            };
            let id = InodeId::from_fsid_and_inodeid(
                self.id.fs_id(),
                get_inode_id(&[&self.tid.value().to_string(), &task.tid.value().to_string()]),
            );
            entries.push(Dirent {
                id,
                offset: (i + 1) as u64,
                file_type: FileType::Directory,
                name: task.pid.in_ns(&self.ns).to_string(),
            });
        }
        Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
