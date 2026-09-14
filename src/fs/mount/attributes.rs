//! Mount-local restrictions and shared-superblock read-only state.
use super::{MOUNT_OPS, Mount};
use crate::{process::user_namespace::UserNamespace, sync::SpinLock};
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use libkernel::error::{FsError, KernelError, Result};

pub const RDONLY: u64 = 1;
pub const NOSUID: u64 = 2;
pub const NODEV: u64 = 4;
pub const NOEXEC: u64 = 8;
pub const SUPPORTED: u64 = RDONLY | NOSUID | NODEV | NOEXEC;

pub struct SuperblockState {
    pub owner: Arc<UserNamespace>,
    readonly: AtomicBool,
    writers: AtomicUsize,
}
impl SuperblockState {
    pub(super) fn get(id: u64, owner: Arc<UserNamespace>, flags: u64) -> Arc<Self> {
        static STATES: SpinLock<BTreeMap<u64, Weak<SuperblockState>>> =
            SpinLock::new(BTreeMap::new());
        let mut states = STATES.lock_save_irq();
        if let Some(state) = states.get(&id).and_then(Weak::upgrade) {
            return state;
        }
        states.retain(|_, state| state.strong_count() != 0);
        let state = Arc::new(Self {
            owner,
            readonly: AtomicBool::new(flags & RDONLY != 0),
            writers: AtomicUsize::new(0),
        });
        states.insert(id, Arc::downgrade(&state));
        state
    }
}

pub struct MountAttributes {
    pub superblock: Arc<SuperblockState>,
    flags: AtomicU64,
    pub locked: u64,
    writers: AtomicUsize,
}
impl MountAttributes {
    pub(super) fn new(sb: Arc<SuperblockState>, flags: u64, locked: u64) -> Self {
        Self {
            superblock: sb,
            flags: AtomicU64::new(flags),
            locked,
            writers: AtomicUsize::new(0),
        }
    }
    pub fn flags(&self) -> u64 {
        self.flags.load(Ordering::Acquire)
    }
    pub fn superblock_readonly(&self) -> bool {
        self.superblock.readonly.load(Ordering::Acquire)
    }
    pub fn effective_flags(&self) -> u64 {
        self.flags()
            | if self.superblock.readonly.load(Ordering::Acquire) {
                RDONLY
            } else {
                0
            }
    }
    /// Called under MOUNT_OPS, together with write-lease acquisition.
    pub(super) fn remount(&self, flags: u64, bind: bool) -> Result<()> {
        if flags & !SUPPORTED != 0 {
            return Err(KernelError::NotSupported);
        }
        if self.locked & !flags != 0 {
            return Err(KernelError::NotPermitted);
        }
        if flags & RDONLY != 0 && self.writers.load(Ordering::Acquire) != 0 {
            return Err(FsError::Busy.into());
        }
        if !bind {
            if flags & RDONLY != 0 && self.superblock.writers.load(Ordering::Acquire) != 0 {
                return Err(FsError::Busy.into());
            }
            self.superblock
                .readonly
                .store(flags & RDONLY != 0, Ordering::Release);
        }
        self.flags.store(flags, Ordering::Release);
        Ok(())
    }
}

/// A writable open file or in-flight metadata mutation holds a lease. A
/// read-only remount cannot race past the check and allow a later disk write.
pub struct WriteLease {
    mount: Arc<Mount>,
}
impl WriteLease {
    pub fn acquire(mount: &Arc<Mount>) -> Result<Self> {
        let _guard = MOUNT_OPS.lock_save_irq();
        if mount.attrs.effective_flags() & RDONLY != 0 {
            return Err(KernelError::ReadOnly);
        }
        mount.attrs.writers.fetch_add(1, Ordering::AcqRel);
        mount
            .attrs
            .superblock
            .writers
            .fetch_add(1, Ordering::AcqRel);
        Ok(Self {
            mount: mount.clone(),
        })
    }
}
impl Drop for WriteLease {
    fn drop(&mut self) {
        self.mount.attrs.writers.fetch_sub(1, Ordering::AcqRel);
        self.mount
            .attrs
            .superblock
            .writers
            .fetch_sub(1, Ordering::AcqRel);
    }
}
