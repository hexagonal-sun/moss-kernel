use crate::drivers::fs::proc::cmdline::ProcCmdlineInode;
use crate::drivers::fs::proc::get_inode_id;
use crate::drivers::fs::proc::meminfo::ProcMeminfoInode;
use crate::drivers::fs::proc::stat::ProcStatInode;
use crate::drivers::fs::proc::task::ProcTaskInode;
use crate::process::thread_group::pid::PidT;
use crate::process::{TASK_LIST, Tid, find_task_by_tid};
use crate::sched::current_work;
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
                id: InodeId::from_fsid_and_inodeid(PROCFS_ID, 0),
                file_type: FileType::Directory,
                permissions: FilePermissions::from_bits_retain(0o555),
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
        if matches!(name, "self" | "thread-self" | "mounts") {
            return Ok(Arc::new(ProcAlias {
                id: InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[name])),
                name: name.into(),
            }));
        }
        // Lookup a PID directory.
        let desc = if name == "stat" {
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
            let pid: PidT = name.parse().map_err(|_| FsError::NotFound)?;
            // Search for the task descriptor.
            find_task_by_tid(Tid::from_pid_t(pid))
                .ok_or(FsError::NotFound)?
                .descriptor()
        };

        Ok(Arc::new(ProcTaskInode::new(
            desc.tid(),
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
        for (tid, _) in task_list
            .iter()
            .filter(|(_, task)| task.upgrade().is_some())
        {
            let name = tid.value().to_string();
            let inode_id = InodeId::from_fsid_and_inodeid(
                PROCFS_ID,
                get_inode_id(&[&tid.value().to_string()]),
            );
            let next_offset = (entries.len() + 1) as u64;
            entries.push(Dirent::new(
                name,
                inode_id,
                FileType::Directory,
                next_offset,
            ));
        }

        for name in ["self", "thread-self", "mounts"] {
            entries.push(Dirent::new(
                name.into(),
                InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[name])),
                FileType::Symlink,
                (entries.len() + 1) as u64,
            ));
        }
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

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

// Stable alias inodes compute their target for each lookup. Caching a task
// directory under the name "self" would retain another caller's captured TID.
struct ProcAlias {
    id: InodeId,
    name: alloc::string::String,
}
#[async_trait]
impl Inode for ProcAlias {
    fn id(&self) -> InodeId {
        self.id
    }
    async fn getattr(&self) -> error::Result<FileAttr> {
        Ok(FileAttr {
            id: self.id,
            file_type: FileType::Symlink,
            permissions: FilePermissions::from_bits_retain(0o777),
            ..Default::default()
        })
    }
    async fn readlink(&self) -> error::Result<libkernel::fs::pathbuf::PathBuf> {
        let task = current_work();
        Ok(match self.name.as_str() {
            "self" => task.process.tgid.value().to_string().into(),
            "thread-self" => {
                alloc::format!("{}/task/{}", task.process.tgid.value(), task.tid.value()).into()
            }
            _ => "self/mounts".into(),
        })
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
