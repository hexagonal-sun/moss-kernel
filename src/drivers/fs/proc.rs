#![allow(clippy::module_name_repetitions)]

mod cmdline;
mod meminfo;
mod root;
mod stat;
mod task;
pub fn open_control(
    inode: &dyn libkernel::fs::Inode,
    opener: &crate::process::creds::Credentials,
) -> libkernel::error::Result<Option<alloc::boxed::Box<dyn crate::fs::fops::FileOps>>> {
    if let Some(ops) = task::id_map::open(inode, opener)? {
        return Ok(Some(ops));
    }
    task::mounts::open(inode)
}
pub(crate) use task::follow_path;

use crate::drivers::{Driver, FilesystemDriver};
use crate::process::pid_namespace::PidNamespace;
use crate::sync::SpinLock;
use alloc::{boxed::Box, sync::Arc};
use alloc::{collections::BTreeMap, sync::Weak};
use async_trait::async_trait;
use core::hash::Hasher;
use libkernel::{
    error::{KernelError, Result},
    fs::{BlockDevice, Filesystem, Inode, PROCFS_ID},
};
use log::warn;
use root::ProcRootInode;

/// Deterministically generates an inode ID for the given path segments within the procfs filesystem.
fn get_inode_id(path_segments: &[&str]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    // Ensure non-collision if other filesystems also use this method
    hasher.write(b"procfs");
    for segment in path_segments {
        hasher.write(segment.as_bytes());
    }
    let hash = hasher.finish();
    assert_ne!(hash, 0, "Generated inode ID cannot be zero");
    hash
}

pub struct ProcFs {
    root: Arc<ProcRootInode>,
    id: u64,
    owner: Arc<crate::process::user_namespace::UserNamespace>,
}

impl ProcFs {
    fn new(id: u64, ns: Arc<PidNamespace>) -> Arc<Self> {
        let root_inode = Arc::new(ProcRootInode::new(id, ns.clone()));
        Arc::new(Self {
            root: root_inode,
            id,
            owner: ns.owner.clone(),
        })
    }
}

#[async_trait]
impl Filesystem for ProcFs {
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Ok(self.root.clone())
    }

    fn id(&self) -> u64 {
        self.id
    }

    fn magic(&self) -> u64 {
        0x9fa0 // procfs magic number
    }
}

// A procfs superblock belongs to the PID namespace active at mount time,
// not to the reader or to pid_for_children. Bind/retained mounts keep that view.
static INSTANCES: SpinLock<BTreeMap<u64, Weak<ProcFs>>> = SpinLock::new(BTreeMap::new());

pub fn superblock_owner(id: u64) -> Option<Arc<crate::process::user_namespace::UserNamespace>> {
    INSTANCES
        .lock_save_irq()
        .values()
        .filter_map(Weak::upgrade)
        .find(|fs| fs.id == id)
        .map(|fs| fs.owner.clone())
}

pub struct ProcFsDriver;

impl ProcFsDriver {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for ProcFsDriver {
    fn name(&self) -> &'static str {
        "procfs"
    }

    fn as_filesystem_driver(self: Arc<Self>) -> Option<Arc<dyn FilesystemDriver>> {
        Some(self)
    }
}

#[async_trait]
impl FilesystemDriver for ProcFsDriver {
    async fn construct(
        &self,
        fs_id: u64,
        device: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        if device.is_some() {
            warn!("procfs should not be constructed with a block device");
            return Err(KernelError::InvalidValue);
        }
        let ns = crate::sched::current_work().pid_ns();
        let mut instances = INSTANCES.lock_save_irq();
        if let Some(fs) = instances.get(&ns.id).and_then(Weak::upgrade) {
            return Ok(fs);
        }
        instances.retain(|_, fs| fs.strong_count() != 0);
        let fs = ProcFs::new(if ns.level == 0 { PROCFS_ID } else { fs_id }, ns.clone());
        instances.insert(ns.id, Arc::downgrade(&fs));
        Ok(fs)
    }
}
