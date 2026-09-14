//! The CLONE_FS sharing domain, replaceable by unshare without changing other
//! tasks. cwd, root and umask must detach together.
use crate::{fs::DummyInode, sync::SpinLock};
use alloc::sync::Arc;
use libkernel::fs::{Inode, pathbuf::PathBuf};

pub struct FsContext {
    pub cwd: SpinLock<(Arc<dyn Inode>, PathBuf)>,
    pub root: SpinLock<(Arc<dyn Inode>, PathBuf)>,
    pub umask: SpinLock<u32>,
}

impl FsContext {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cwd: SpinLock::new((Arc::new(DummyInode {}), PathBuf::new())),
            root: SpinLock::new((Arc::new(DummyInode {}), PathBuf::new())),
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
}
