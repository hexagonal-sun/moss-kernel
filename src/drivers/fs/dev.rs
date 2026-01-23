use crate::drivers::{Driver, FilesystemDriver};
use crate::sync::{OnceLock, SpinLock};
use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
};
use async_trait::async_trait;
use core::sync::atomic::{AtomicU64, Ordering};
use libkernel::fs::attr::{FileAttr, FilePermissions};
use libkernel::fs::{BlockDevice, DirStream, Dirent, Filesystem};
use libkernel::{
    driver::CharDevDescriptor,
    error::{FsError, KernelError, Result},
    fs::{DEVFS_ID, FileType, Inode, InodeId},
};
use log::warn;

pub struct DevFs {
    root: Arc<DevFsINode>,
    next_inode_id: AtomicU64,
}

impl DevFs {
    pub fn new() -> Arc<Self> {
        let root_inode = Arc::new(DevFsINode {
            id: InodeId::from_fsid_and_inodeid(DEVFS_ID, 0),
            attr: SpinLock::new(FileAttr {
                file_type: FileType::Directory,
                mode: FilePermissions::from_bits_retain(0o755),
                ..FileAttr::default()
            }),
            kind: InodeKind::Directory(SpinLock::new(BTreeMap::new())),
        });

        Arc::new(Self {
            root: root_inode,
            next_inode_id: AtomicU64::new(1),
        })
    }

    pub fn mknod(
        &self,
        name: String,
        device_id: CharDevDescriptor,
        mode: FilePermissions,
    ) -> Result<()> {
        let InodeKind::Directory(ref children) = self.root.kind else {
            // This should be impossible as the root is always a directory.
            return Err(FsError::InvalidFs.into());
        };

        let mut children = children.lock_save_irq();
        if children.contains_key(&name) {
            return Err(KernelError::InUse);
        }

        let id = self.next_inode_id.fetch_add(1, Ordering::SeqCst);

        let new_inode = Arc::new(DevFsINode {
            id: InodeId::from_fsid_and_inodeid(DEVFS_ID, id),
            attr: SpinLock::new(FileAttr {
                file_type: FileType::CharDevice(device_id),
                mode,
                ..FileAttr::default()
            }),
            // This is the crucial part: we store the device handle.
            kind: InodeKind::CharDevice { device_id },
        });

        children.insert(name.to_string(), new_inode);
        Ok(())
    }
}

#[async_trait]
impl Filesystem for DevFs {
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        // The root inode is now stored directly in the struct.
        Ok(self.root.clone())
    }

    fn id(&self) -> u64 {
        DEVFS_ID
    }
}

enum InodeKind {
    /// A directory, which contains a map of names to child inodes.
    Directory(SpinLock<BTreeMap<String, Arc<DevFsINode>>>),
    /// A character device, which stores its major/minor handle (`dev_t`).
    CharDevice { device_id: CharDevDescriptor },
}

struct DevDirStreamer {
    children: BTreeMap<String, Arc<DevFsINode>>,
    idx: usize,
}

#[async_trait]
impl DirStream for DevDirStreamer {
    async fn next_entry(&mut self) -> Result<Option<Dirent>> {
        if let Some((name, inode)) = self.children.iter().nth(self.idx) {
            self.idx += 1;

            Ok(Some(Dirent {
                id: inode.id,
                name: name.clone(),
                file_type: inode.attr.lock_save_irq().file_type,
                offset: self.idx as u64,
            }))
        } else {
            Ok(None)
        }
    }
}

/// Represents an Inode within `devfs`. This is now a self-contained metadata object.
struct DevFsINode {
    id: InodeId,
    attr: SpinLock<FileAttr>,
    kind: InodeKind,
}

#[async_trait]
impl Inode for DevFsINode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        match &self.kind {
            InodeKind::Directory(children) => {
                let children = children.lock_save_irq();
                children
                    .get(name)
                    .map(|inode| inode.clone() as Arc<dyn Inode>)
                    .ok_or_else(|| FsError::NotFound.into())
            }
            InodeKind::CharDevice { .. } => Err(FsError::NotADirectory.into()),
        }
    }

    async fn getattr(&self) -> Result<FileAttr> {
        let mut attr = self.attr.lock_save_irq().clone();
        if let InodeKind::CharDevice { device_id } = self.kind {
            attr.file_type = FileType::CharDevice(device_id);
        }
        Ok(attr)
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        match &self.kind {
            InodeKind::Directory(children) => {
                let children = children.lock_save_irq().clone();
                Ok(Box::new(DevDirStreamer {
                    children,
                    idx: start_offset as usize,
                }))
            }
            InodeKind::CharDevice { .. } => Err(FsError::NotADirectory.into()),
        }
    }
}

/// The driver for the `devfs` filesystem itself.
pub struct DevFsDriver {}

impl DevFsDriver {
    pub fn new() -> Self {
        Self {}
    }
}

impl Driver for DevFsDriver {
    fn name(&self) -> &'static str {
        "devfs"
    }

    fn as_filesystem_driver(self: Arc<Self>) -> Option<Arc<dyn FilesystemDriver>> {
        Some(self)
    }
}

#[async_trait]
impl FilesystemDriver for DevFsDriver {
    async fn construct(
        &self,
        _fs_id: u64,
        device: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        if device.is_some() {
            warn!("devfs should have no backing store");

            return Err(KernelError::InvalidValue);
        }
        Ok(devfs())
    }
}

/// The single, global instance of the device filesystem.
static DEVFS_INSTANCE: OnceLock<Arc<DevFs>> = OnceLock::new();

/// Initializes and/or returns the global singleton `DevFs` instance.
/// This is the main entry point for the rest of the kernel to interact with devfs.
pub fn devfs() -> Arc<DevFs> {
    DEVFS_INSTANCE
        .get_or_init(|| {
            log::info!("devfs initialized");
            DevFs::new()
        })
        .clone()
}
