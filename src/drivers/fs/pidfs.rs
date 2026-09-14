//! The single, kernel-only filesystem backing pidfds.

use crate::{fs::VFS, process::pid_namespace::PidIdentity, sync::OnceLock};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{
        Filesystem, Inode, InodeId, PIDFS_ID,
        attr::{FileAttr, FilePermissions},
        stats::{FilesystemStats, ST_VALID},
    },
    memory::PAGE_SIZE,
};

const PID_FS_MAGIC: u64 = 0x5049_4446;
const NAME_MAX: u64 = 255;

pub struct PidFs;

#[async_trait]
impl Filesystem for PidFs {
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        // No path-addressable root and no userspace mount driver.
        Err(KernelError::NotSupported)
    }

    fn id(&self) -> u64 {
        PIDFS_ID
    }

    fn magic(&self) -> u64 {
        PID_FS_MAGIC
    }

    async fn statfs(&self) -> Result<FilesystemStats> {
        // Linux pidfs uses simple_statfs. Capacity/inode counters are zero;
        // they do not count live processes. The internal mount has ST_VALID
        // but no user mount flags. VFS fills fragment_size from block_size.
        Ok(FilesystemStats {
            magic: self.magic(),
            id: self.id(),
            block_size: PAGE_SIZE as u64,
            name_length: NAME_MAX,
            flags: ST_VALID,
            ..Default::default()
        })
    }
}

static PIDFS_INSTANCE: OnceLock<Arc<PidFs>> = OnceLock::new();

pub fn new_inode(pid: Arc<PidIdentity>) -> Arc<dyn Inode> {
    PIDFS_INSTANCE.get_or_init(|| {
        let fs = Arc::new(PidFs);
        VFS.register_internal_fs(fs.clone());
        fs
    });
    Arc::new(PidInode {
        // MOSS allocates task IDs monotonically. Keep the inode independent
        // of the task table so an open pidfd survives target exit/reaping.
        id: InodeId::from_fsid_and_inodeid(PIDFS_ID, pid.global.0 as u64),
        _pid: pid,
    })
}

struct PidInode {
    id: InodeId,
    _pid: Arc<PidIdentity>,
}

#[async_trait]
impl Inode for PidInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(FileAttr {
            id: self.id,
            block_size: PAGE_SIZE as u32,
            permissions: FilePermissions::S_IRUSR
                | FilePermissions::S_IWUSR
                | FilePermissions::S_IXUSR,
            ..Default::default()
        })
    }

    async fn lookup(&self, _name: &str) -> Result<Arc<dyn Inode>> {
        Err(FsError::NotADirectory.into())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
