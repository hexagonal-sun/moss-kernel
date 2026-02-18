use crate::drivers::fs::proc::{get_inode_id, procfs};
use crate::process::fd_table::Fd;
use crate::process::{TaskDescriptor, find_task_by_descriptor};
use crate::sched::current::current_task_shared;
use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::error::Result;
use libkernel::error::{FsError, KernelError};
use libkernel::fs::attr::FileAttr;
use libkernel::fs::pathbuf::PathBuf;
use libkernel::fs::{
    DirStream, Dirent, FileType, Filesystem, Inode, InodeId, SimpleDirStream, SimpleFile,
};

pub struct ProcFdInode {
    id: InodeId,
    attr: FileAttr,
    desc: TaskDescriptor,
    fd_info: bool,
}

impl ProcFdInode {
    pub fn new(desc: TaskDescriptor, fd_info: bool, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: FileType::Directory,
                // Define appropriate file attributes for fdinfo.
                ..FileAttr::default()
            },
            desc,
            fd_info,
        }
    }

    fn dir_name(&self) -> &str {
        if self.fd_info { "fdinfo" } else { "fd" }
    }
}

#[async_trait]
impl Inode for ProcFdInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        let fd: i32 = name.parse().map_err(|_| FsError::NotFound)?;
        let task = current_task_shared();
        let fd_table = task.fd_table.lock_save_irq();
        if fd_table.get(Fd(fd)).is_none() {
            return Err(FsError::NotFound.into());
        }
        let fs = procfs();
        let inode_id = InodeId::from_fsid_and_inodeid(
            fs.id(),
            get_inode_id(&[&self.desc.tid().value().to_string(), self.dir_name(), name]),
        );
        Ok(Arc::new(ProcFdFile::new(
            self.desc,
            self.fd_info,
            fd,
            inode_id,
        )))
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let task = find_task_by_descriptor(&self.desc).ok_or(FsError::NotFound)?;
        let fd_table = task.fd_table.lock_save_irq();
        let mut entries = Vec::new();
        for fd in 0..fd_table.len() {
            if fd_table.get(Fd(fd as i32)).is_none() {
                continue;
            }
            let fd_str = fd.to_string();
            let next_offset = (entries.len() + 1) as u64;
            entries.push(Dirent {
                id: InodeId::from_fsid_and_inodeid(
                    self.id.fs_id(),
                    get_inode_id(&[
                        &self.desc.tid().value().to_string(),
                        self.dir_name(),
                        &fd_str,
                    ]),
                ),
                offset: next_offset,
                file_type: FileType::File,
                name: fd_str,
            });
        }

        Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
    }
}

// TODO: Support fd links in /proc/[pid]/fd/

pub struct ProcFdFile {
    id: InodeId,
    attr: FileAttr,
    desc: TaskDescriptor,
    fd_info: bool,
    fd: i32,
}

impl ProcFdFile {
    pub fn new(desc: TaskDescriptor, fd_info: bool, fd: i32, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: if fd_info {
                    FileType::File
                } else {
                    FileType::Symlink
                },
                // Define appropriate file attributes for fdinfo file.
                ..FileAttr::default()
            },
            desc,
            fd_info,
            fd,
        }
    }
}

#[async_trait]
impl SimpleFile for ProcFdFile {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn read(&self) -> Result<Vec<u8>> {
        let task = find_task_by_descriptor(&self.desc).ok_or(FsError::NotFound)?;
        let fd_entry = task
            .fd_table
            .lock_save_irq()
            .get(Fd(self.fd))
            .ok_or(FsError::NotFound)?;
        let (_, ctx) = &mut *fd_entry.lock().await;
        let info_string = format!("pos: {}\nflags: {}", ctx.pos, ctx.flags.bits());
        if self.fd_info {
            Ok(info_string.into_bytes())
        } else {
            Err(KernelError::NotSupported)
        }
    }

    async fn readlink(&self) -> Result<PathBuf> {
        if !self.fd_info {
            if let Some(task) = find_task_by_descriptor(&self.desc) {
                let Some(file) = task.fd_table.lock_save_irq().get(Fd(self.fd)) else {
                    return Err(FsError::NotFound.into());
                };
                if let Some(path) = file.path() {
                    Ok(path.to_owned())
                } else {
                    // TODO: Find file type
                    todo!(
                        "Implement readlink for /proc/[pid]/fd/[fd] when fd doesn't refer to a file with an inode"
                    )
                }
            } else {
                Err(FsError::NotFound.into())
            }
        } else {
            Err(KernelError::NotSupported)
        }
    }
}
