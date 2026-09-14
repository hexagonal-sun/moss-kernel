use crate::process::{Tid, find_task_by_tid, user_namespace::UserNamespace};
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
        if name != "user" {
            return Err(FsError::NotFound.into());
        }
        Ok(Arc::new(NsLink {
            tid: self.tid,
            id: InodeId::from_fsid_and_inodeid(
                self.id.fs_id(),
                super::super::get_inode_id(&[&format!("{}", self.tid.value()), "ns", "user"]),
            ),
        }))
    }
    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let node = self.lookup("user").await?;
        Ok(Box::new(SimpleDirStream::new(
            vec![Dirent::new("user".into(), node.id(), FileType::Symlink, 1)],
            start_offset,
        )))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

struct NsLink {
    tid: Tid,
    id: InodeId,
}
impl NsLink {
    fn namespace(&self) -> Result<Arc<UserNamespace>> {
        let task = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
        let caller = crate::sched::current_work().task.t_shared.clone();
        crate::process::access::ptrace_may_access(&caller, &task, true)
            .map_err(|_| FsError::PermissionDenied)?;
        Ok(task.creds.lock_save_irq().user_ns())
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
        Ok(format!("user:[{}]", self.namespace()?.id).into())
    }
    async fn follow_link(&self) -> Result<Option<Arc<dyn Inode>>> {
        Ok(Some(crate::drivers::fs::nsfs::new_inode(self.namespace()?)))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
