use crate::drivers::fs::proc::cmdline::ProcCmdlineInode;
use crate::drivers::fs::proc::get_inode_id;
use crate::drivers::fs::proc::meminfo::ProcMeminfoInode;
use crate::drivers::fs::proc::stat::ProcStatInode;
use crate::drivers::fs::proc::task::ProcTaskInode;
use crate::process::{TASK_LIST, TaskDescriptor, Tid};
use crate::sched::current::current_task;
use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::error;
use libkernel::error::FsError;
use libkernel::fs::attr::{FileAttr, FilePermissions};
use libkernel::fs::{DirStream, Dirent, FileType, Inode, InodeId, PROCFS_ID, SimpleDirStream};

pub struct ProcRootInode {
    id: InodeId,
    attr: FileAttr,
}

impl ProcRootInode {
    pub fn new() -> Self {
        Self {
            id: InodeId::from_fsid_and_inodeid(PROCFS_ID, 0),
            attr: FileAttr {
                file_type: FileType::Directory,
                mode: FilePermissions::from_bits_retain(0o555),
                ..FileAttr::default()
            },
        }
    }
}

#[async_trait]
impl Inode for ProcRootInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn lookup(&self, name: &str) -> error::Result<Arc<dyn Inode>> {
        // Lookup a PID directory.
        let desc = if name == "self" {
            let current_task = current_task();
            TaskDescriptor::from_tgid_tid(current_task.pgid(), Tid::from_tgid(current_task.pgid()))
        } else if name == "thread-self" {
            let current_task = current_task();
            current_task.descriptor()
        } else if name == "stat" {
            return Ok(Arc::new(ProcStatInode::new(
                InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&["stat"])),
            )));
        } else if name == "meminfo" {
            return Ok(Arc::new(ProcMeminfoInode::new(
                InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&["meminfo"])),
            )));
        } else if name == "cmdline" {
            return Ok(Arc::new(ProcCmdlineInode::new(
                InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&["cmdline"])),
            )));
        } else {
            let pid: u32 = name.parse().map_err(|_| FsError::NotFound)?;
            // Search for the task descriptor.
            TASK_LIST
                .lock_save_irq()
                .keys()
                .find(|d| d.tgid().value() == pid)
                .cloned()
                .ok_or(FsError::NotFound)?
        };

        Ok(Arc::new(ProcTaskInode::new(
            desc,
            false,
            InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&[name])),
        )))
    }

    async fn getattr(&self) -> error::Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn readdir(&self, start_offset: u64) -> error::Result<Box<dyn DirStream>> {
        let mut entries: Vec<Dirent> = Vec::new();
        // Gather task list under interrupt-safe lock.
        let task_list = TASK_LIST.lock_save_irq();
        for (desc, _) in task_list
            .iter()
            .filter(|(_, task)| task.upgrade().is_some())
        {
            let name = desc.tgid().value().to_string();
            let inode_id = InodeId::from_fsid_and_inodeid(
                PROCFS_ID,
                get_inode_id(&[&desc.tgid().value().to_string()]),
            );
            let next_offset = (entries.len() + 1) as u64;
            entries.push(Dirent::new(
                name,
                inode_id,
                FileType::Directory,
                next_offset,
            ));
        }
        let current_task = current_task();
        entries.push(Dirent::new(
            "self".to_string(),
            InodeId::from_fsid_and_inodeid(
                PROCFS_ID,
                get_inode_id(&[&current_task.descriptor().tgid().value().to_string()]),
            ),
            FileType::Directory,
            (entries.len() + 1) as u64,
        ));
        entries.push(Dirent::new(
            "thread-self".to_string(),
            InodeId::from_fsid_and_inodeid(
                PROCFS_ID,
                get_inode_id(&[&current_task.descriptor().tid().value().to_string()]),
            ),
            FileType::Directory,
            (entries.len() + 1) as u64,
        ));
        entries.push(Dirent::new(
            "stat".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&["stat"])),
            FileType::File,
            (entries.len() + 1) as u64,
        ));
        entries.push(Dirent::new(
            "meminfo".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&["meminfo"])),
            FileType::File,
            (entries.len() + 1) as u64,
        ));
        entries.push(Dirent::new(
            "cmdline".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&["cmdline"])),
            FileType::File,
            (entries.len() + 1) as u64,
        ));

        Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
    }
}
