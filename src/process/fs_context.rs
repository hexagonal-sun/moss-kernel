//! The CLONE_FS sharing domain, replaceable by unshare without changing other
//! tasks. cwd, root and umask must detach together.
use crate::{
    fs::{DummyInode, VfsPath},
    sync::SpinLock,
};
use alloc::sync::Arc;

pub struct FsContext {
    pub cwd: SpinLock<VfsPath>,
    pub root: SpinLock<VfsPath>,
    pub umask: SpinLock<u32>,
}

impl FsContext {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cwd: SpinLock::new(VfsPath::anonymous(Arc::new(DummyInode {}))),
            root: SpinLock::new(VfsPath::anonymous(Arc::new(DummyInode {}))),
            umask: SpinLock::new(0),
        })
    }

    pub fn duplicate(&self) -> Arc<Self> {
        Arc::new(Self {
            cwd: SpinLock::new(self.cwd.lock_save_irq().clone()),
            root: SpinLock::new(self.root.lock_save_irq().clone()),
            umask: SpinLock::new(*self.umask.lock_save_irq()),
        })
    }

    pub fn remap_mounts(
        &self,
        map: &alloc::collections::BTreeMap<u64, Arc<crate::fs::mount::Mount>>,
    ) {
        for slot in [&self.cwd, &self.root] {
            let mut path = slot.lock_save_irq();
            if let Some(mount) = map.get(&path.mount_id()) {
                *path = VfsPath::new(Some(mount.clone()), path.dentry.clone());
            }
        }
    }
}
