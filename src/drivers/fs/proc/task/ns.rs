use crate::drivers::fs::nsfs::Namespace;
use crate::process::{Tid, find_task_by_tid};
use alloc::{boxed::Box, format, sync::Arc, vec};
use async_trait::async_trait;
use libkernel::{
    error::{FsError, Result},
    fs::{
        DirStream, Dirent, FileType, Inode, InodeId, SimpleDirStream,
        attr::{FileAttr, FilePermissions},
        pathbuf::PathBuf,
    },
};

pub struct NsDir {
    pub tid: Tid,
    pub id: InodeId,
}
#[async_trait]
impl Inode for NsDir {
    fn id(&self) -> InodeId {
        self.id
    }
    async fn getattr(&self) -> Result<FileAttr> {
        Ok(FileAttr {
            id: self.id,
            file_type: FileType::Directory,
            permissions: FilePermissions::from_bits_retain(0o511),
            ..Default::default()
        })
    }
    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        if !matches!(name, "user" | "mnt" | "pid" | "pid_for_children") {
            return Err(FsError::NotFound.into());
        }
        Ok(Arc::new(NsLink {
            tid: self.tid,
            kind: match name {
                "mnt" => 1,
                "pid" => 2,
                "pid_for_children" => 3,
                _ => 0,
            },
            id: InodeId::from_fsid_and_inodeid(
                self.id.fs_id(),
                super::super::get_inode_id(&[&format!("{}", self.tid.value()), "ns", name]),
            ),
        }))
    }
    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let mut entries = vec![];
        for (i, name) in ["user", "mnt", "pid", "pid_for_children"]
            .iter()
            .enumerate()
        {
            let node = self.lookup(name).await?;
            entries.push(Dirent::new(
                (*name).into(),
                node.id(),
                FileType::Symlink,
                i as u64 + 1,
            ));
        }
        Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

struct NsLink {
    tid: Tid,
    kind: u8,
    id: InodeId,
}
impl NsLink {
    fn namespace(&self) -> Result<Namespace> {
        let task = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
        let caller = crate::sched::current_work().task.t_shared.clone();
        crate::process::access::ptrace_may_access(&caller, &task, true)
            .map_err(|_| FsError::PermissionDenied)?;
        Ok(match self.kind {
            1 => Namespace::Mount(task.mount_ns()),
            2 => Namespace::Pid(task.pid_ns()),
            3 => {
                let ns = task.child_pid_ns();
                if !ns.initialized() {
                    return Err(FsError::NotFound.into());
                }
                Namespace::Pid(ns)
            }
            _ => Namespace::User(task.creds.lock_save_irq().user_ns()),
        })
    }
}
#[async_trait]
impl Inode for NsLink {
    fn id(&self) -> InodeId {
        self.id
    }
    async fn getattr(&self) -> Result<FileAttr> {
        Ok(FileAttr {
            id: self.id,
            file_type: FileType::Symlink,
            permissions: FilePermissions::from_bits_retain(0o777),
            ..Default::default()
        })
    }
    async fn readlink(&self) -> Result<PathBuf> {
        let ns = self.namespace()?;
        Ok(format!("{}:[{}]", ns.name(), ns.id()).into())
    }
    async fn follow_link(&self) -> Result<Option<Arc<dyn Inode>>> {
        Ok(Some(crate::drivers::fs::nsfs::namespace_inode(
            self.namespace()?,
        )))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
