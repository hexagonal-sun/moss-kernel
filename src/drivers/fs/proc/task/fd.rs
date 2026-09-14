use crate::drivers::fs::proc::{get_inode_id, procfs};
use crate::process::fd_table::Fd;
use crate::process::{Tid, find_task_by_tid};
use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use libkernel::error::Result;
use libkernel::error::{FsError, KernelError};
use libkernel::fs::attr::{FileAttr, FilePermissions};
use libkernel::fs::pathbuf::PathBuf;
use libkernel::fs::{
    DirStream, Dirent, FileType, Filesystem, Inode, InodeId, SimpleDirStream, SimpleFile,
};

fn check_access(target: &crate::process::Task) -> Result<()> {
    let caller = crate::sched::current_work().task.t_shared.clone();
    crate::process::access::ptrace_may_access(&caller, target, true)
        .map_err(|_| FsError::PermissionDenied.into())
}

fn task_attr(tid: Tid, mut attr: FileAttr) -> Result<FileAttr> {
    let task = find_task_by_tid(tid).ok_or(FsError::NotFound)?;
    let creds = task.creds.lock_save_irq();
    if task
        .process
        .dumpable
        .load(core::sync::atomic::Ordering::Acquire)
        == 1
    {
        attr.uid = creds.euid();
        attr.gid = creds.egid();
    } else {
        attr.uid = creds
            .user_ns()
            .make_uid(0)
            .unwrap_or(libkernel::proc::ids::Uid::new_root());
        attr.gid = creds
            .user_ns()
            .make_gid(0)
            .unwrap_or(libkernel::proc::ids::Gid::new_root_group());
    }
    Ok(attr)
}

pub struct ProcFdInode {
    id: InodeId,
    attr: FileAttr,
    tid: Tid,
    fd_info: bool,
}

impl ProcFdInode {
    pub fn new(tid: Tid, fd_info: bool, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: FileType::Directory,
                permissions: FilePermissions::from_bits_retain(0o500),
                ..FileAttr::default()
            },
            tid,
            fd_info,
        }
    }

    fn dir_name(&self) -> &str {
        if self.fd_info { "fdinfo" } else { "fd" }
    }
}

#[async_trait]
impl Inode for ProcFdInode {
    async fn check_access(
        &self,
        uid: libkernel::proc::ids::Uid,
        gid: libkernel::proc::ids::Gid,
        groups: &[libkernel::proc::ids::Gid],
        caps: libkernel::proc::caps::Capabilities,
        mode: libkernel::fs::attr::AccessMode,
    ) -> Result<()> {
        let result = self
            .getattr()
            .await?
            .check_access_with_groups(uid, gid, groups, caps, mode);
        if result.is_err() {
            let target = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
            if crate::sched::current_work().task.process.tgid == target.process.tgid {
                return Ok(());
            }
        }
        result
    }

    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> Result<FileAttr> {
        task_attr(self.tid, self.attr.clone())
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        let fd: i32 = name.parse().map_err(|_| FsError::NotFound)?;
        let task = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
        let fd_table = task.fd_table.lock_save_irq();
        if fd_table.get_raw(Fd(fd)).is_none() {
            return Err(FsError::NotFound.into());
        }
        let fs = procfs();
        let inode_id = InodeId::from_fsid_and_inodeid(
            fs.id(),
            get_inode_id(&[&self.tid.value().to_string(), self.dir_name(), name]),
        );
        Ok(Arc::new(ProcFdFile::new(
            self.tid,
            self.fd_info,
            fd,
            inode_id,
        )))
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let task = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
        check_access(&task)?;
        let fd_table = task.fd_table.lock_save_irq();
        let mut entries = Vec::new();
        for (fd, _) in fd_table.iter() {
            let fd_str = fd.as_raw().to_string();
            let next_offset = (entries.len() + 1) as u64;
            entries.push(Dirent {
                id: InodeId::from_fsid_and_inodeid(
                    self.id.fs_id(),
                    get_inode_id(&[&self.tid.value().to_string(), self.dir_name(), &fd_str]),
                ),
                offset: next_offset,
                file_type: FileType::File,
                name: fd_str,
            });
        }

        Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

pub struct ProcFdFile {
    id: InodeId,
    attr: FileAttr,
    tid: Tid,
    fd_info: bool,
    fd: i32,
}

impl ProcFdFile {
    pub fn new(tid: Tid, fd_info: bool, fd: i32, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: if fd_info {
                    FileType::File
                } else {
                    FileType::Symlink
                },
                permissions: FilePermissions::from_bits_retain(if fd_info { 0o400 } else { 0o700 }),
                ..FileAttr::default()
            },
            tid,
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
        task_attr(self.tid, self.attr.clone())
    }

    async fn read(&self) -> Result<Vec<u8>> {
        let task = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
        check_access(&task)?;
        let fd_entry = task
            .fd_table
            .lock_save_irq()
            .get_raw(Fd(self.fd))
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
            if let Some(task) = find_task_by_tid(self.tid) {
                check_access(&task)?;
                let Some(file) = task.fd_table.lock_save_irq().get_raw(Fd(self.fd)) else {
                    return Err(FsError::NotFound.into());
                };
                if let Some(path) = file.path() {
                    Ok(path.to_owned())
                } else {
                    let (ops, _) = &*file.lock().await;
                    ops.anonymous_name()
                        .map(PathBuf::from)
                        .ok_or(KernelError::NotSupported)
                }
            } else {
                Err(FsError::NotFound.into())
            }
        } else {
            Err(KernelError::NotSupported)
        }
    }

    async fn follow_link(&self) -> Result<Option<Arc<dyn Inode>>> {
        if self.fd_info {
            return Ok(None);
        }
        let task = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
        check_access(&task)?;
        let file = task
            .fd_table
            .lock_save_irq()
            .get_raw(Fd(self.fd))
            .ok_or(FsError::NotFound)?;
        Ok(file.inode())
    }
}
