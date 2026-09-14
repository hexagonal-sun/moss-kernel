use crate::drivers::fs::proc::cmdline::ProcCmdlineInode;
use crate::drivers::fs::proc::get_inode_id;
use crate::drivers::fs::proc::meminfo::ProcMeminfoInode;
use crate::drivers::fs::proc::stat::ProcStatInode;
use crate::drivers::fs::proc::task::ProcTaskInode;
use crate::process::thread_group::pid::PidT;
use crate::process::{TASK_LIST, find_task_by_tid};
use crate::sched::current_work;
use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::error;
use libkernel::error::FsError;
use libkernel::fs::attr::{FileAttr, FilePermissions};
use libkernel::fs::{DirStream, Dirent, FileType, Inode, InodeId, SimpleDirStream};

pub struct ProcRootInode {
    id: InodeId,
    attr: FileAttr,
    ns: Arc<crate::process::pid_namespace::PidNamespace>,
}

impl ProcRootInode {
    pub fn new(fs_id: u64, ns: Arc<crate::process::pid_namespace::PidNamespace>) -> Self {
        Self {
            ns,
            id: InodeId::from_fsid_and_inodeid(fs_id, 0),
            attr: FileAttr {
                id: InodeId::from_fsid_and_inodeid(fs_id, 0),
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
                id: InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&[name])),
                name: name.into(),
                ns: self.ns.clone(),
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
            find_task_by_tid(self.ns.resolve(pid as u32).ok_or(FsError::NotFound)?)
                .ok_or(FsError::NotFound)?
                .descriptor()
        };

        Ok(Arc::new(ProcTaskInode::new(
            desc.tid(),
            false,
            self.ns.clone(),
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
        for task in task_list.values() {
            let Some(task) = task.upgrade() else {
                continue;
            };
            let number = task.pid.in_ns(&self.ns);
            if number == 0 || task.tid.0 != task.process.tgid.0 {
                continue;
            }
            let name = number.to_string();
            let inode_id = InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&[&name]));
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
                InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&[name])),
                FileType::Symlink,
                (entries.len() + 1) as u64,
            ));
        }
        entries.push(Dirent::new(
            "stat".to_string(),
            InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&["stat"])),
            FileType::File,
            (entries.len() + 1) as u64,
        ));
        entries.push(Dirent::new(
            "meminfo".to_string(),
            InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&["meminfo"])),
            FileType::File,
            (entries.len() + 1) as u64,
        ));
        entries.push(Dirent::new(
            "cmdline".to_string(),
            InodeId::from_fsid_and_inodeid(self.id.fs_id(), get_inode_id(&["cmdline"])),
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
    ns: Arc<crate::process::pid_namespace::PidNamespace>,
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
        let pid = task.process.pid.in_ns(&self.ns);
        let tid = task.pid.in_ns(&self.ns);
        if pid == 0 {
            return Err(FsError::NotFound.into());
        }
        Ok(match self.name.as_str() {
            "self" => pid.to_string().into(),
            "thread-self" => alloc::format!("{pid}/task/{tid}").into(),
            _ => "self/mounts".into(),
        })
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
