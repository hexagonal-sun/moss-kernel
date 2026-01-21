mod fd;
// TODO: allowlist this across the codebase
#[expect(clippy::module_inception)]
mod task;
mod task_file;

use crate::drivers::fs::proc::task::task_file::{ProcTaskFileInode, TaskFileType};
use crate::drivers::fs::proc::{get_inode_id, procfs};
use crate::process::TaskDescriptor;
use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::error::FsError;
use libkernel::fs::attr::{FileAttr, FilePermissions};
use libkernel::fs::{
    DirStream, Dirent, FileType, Filesystem, Inode, InodeId, PROCFS_ID, SimpleDirStream,
};

pub struct ProcTaskInode {
    id: InodeId,
    attr: FileAttr,
    desc: TaskDescriptor,
    is_task_dir: bool,
}

impl ProcTaskInode {
    pub fn new(desc: TaskDescriptor, is_task_dir: bool, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: FileType::Directory,
                mode: FilePermissions::from_bits_retain(0o555),
                ..FileAttr::default()
            },
            desc,
            is_task_dir,
        }
    }
}

#[async_trait]
impl Inode for ProcTaskInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn lookup(&self, name: &str) -> libkernel::error::Result<Arc<dyn Inode>> {
        let fs = procfs();
        let inode_id = InodeId::from_fsid_and_inodeid(
            fs.id(),
            get_inode_id(&[&self.desc.tid().value().to_string(), name]),
        );
        if name == "fdinfo" {
            return Ok(Arc::new(fd::ProcFdInode::new(self.desc, true, inode_id)));
        } else if name == "fd" {
            return Ok(Arc::new(fd::ProcFdInode::new(self.desc, false, inode_id)));
        } else if name == "task"
            && self.desc.tid().value() == self.desc.tgid().value()
            && !self.is_task_dir
        {
            return Ok(Arc::new(task::ProcTaskDirInode::new(
                self.desc.tgid(),
                inode_id,
            )));
        }
        if let Ok(file_type) = TaskFileType::try_from(name) {
            Ok(Arc::new(ProcTaskFileInode::new(
                self.desc.tid(),
                file_type,
                self.is_task_dir,
                inode_id,
            )))
        } else {
            Err(FsError::NotFound.into())
        }
    }

    async fn getattr(&self) -> libkernel::error::Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn readdir(&self, start_offset: u64) -> libkernel::error::Result<Box<dyn DirStream>> {
        let mut entries: Vec<Dirent> = Vec::new();
        let initial_str = self.desc.tid().value().to_string();
        entries.push(Dirent::new(
            "status".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[&initial_str, "status"])),
            FileType::File,
            1,
        ));
        entries.push(Dirent::new(
            "comm".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[&initial_str, "comm"])),
            FileType::File,
            2,
        ));
        entries.push(Dirent::new(
            "state".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[&initial_str, "state"])),
            FileType::File,
            3,
        ));
        entries.push(Dirent::new(
            "cwd".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[&initial_str, "cwd"])),
            FileType::Symlink,
            4,
        ));
        entries.push(Dirent::new(
            "stat".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[&initial_str, "stat"])),
            FileType::File,
            5,
        ));
        entries.push(Dirent::new(
            "fd".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[&initial_str, "fd"])),
            FileType::Directory,
            6,
        ));
        entries.push(Dirent::new(
            "fdinfo".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[&initial_str, "fdinfo"])),
            FileType::Directory,
            7,
        ));
        if self.desc.tid().value() == self.desc.tgid().value() && !self.is_task_dir {
            entries.push(Dirent::new(
                "task".to_string(),
                InodeId::from_fsid_and_inodeid(PROCFS_ID, get_inode_id(&[&initial_str, "task"])),
                FileType::Directory,
                8,
            ));
        }

        Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
    }
}
