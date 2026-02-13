use crate::drivers::Driver;
use crate::fs::FilesystemDriver;
use crate::sync::OnceLock;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::hash::Hasher;
use libkernel::error::FsError;
use libkernel::fs::attr::FileAttr;
use libkernel::fs::{
    BlockDevice, CGROUPFS_ID, DirStream, Dirent, FileType, Inode, InodeId, SimpleDirStream,
};
use libkernel::{
    error::{KernelError, Result},
    fs::Filesystem,
};
use log::warn;

/// Deterministically generates an inode ID for the given path segments within the sysfs filesystem.
fn get_inode_id(path_segments: &[&str]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    // Ensure non-collision if other filesystems also use this method
    hasher.write(b"cgroupfs");
    for segment in path_segments {
        hasher.write(segment.as_bytes());
    }
    let hash = hasher.finish();
    assert_ne!(hash, 0, "Generated inode ID cannot be zero");
    hash
}

macro_rules! static_dir {
    ($name:ident, $path:expr, $( $entry_name:expr => $entry_type:expr, $entry_ident:ident ),* $(,)? ) => {
        struct $name {
            id: InodeId,
            attr: FileAttr,
        }

        impl $name {
            fn new(id: InodeId) -> Self {
                Self {
                    id,
                    attr: FileAttr {
                        file_type: FileType::Directory,
                        ..FileAttr::default()
                    },
                }
            }

            const fn num_entries() -> usize {
                0 $(+ { let _ = $entry_name; 1 })*
            }
        }

        #[async_trait]
        impl Inode for $name {
            fn id(&self) -> InodeId {
                self.id
            }

            async fn getattr(&self) -> Result<FileAttr> {
                Ok(self.attr.clone())
            }

            async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
                match name {
                    $(
                        $entry_name => {
                            let inode_id = InodeId::from_fsid_and_inodeid(
                                SYSFS_ID,
                                get_inode_id(&[$path, $entry_name]),
                            );
                            Ok(Arc::new($entry_ident::new(inode_id)))
                        },
                    )*
                    _ => Err(KernelError::Fs(FsError::NotFound)),
                }
            }

            async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
                #[allow(unused_mut)]
                let mut entries: Vec<Dirent> = Vec::with_capacity(Self::num_entries());
                $(
                    entries.push(Dirent {
                        id: InodeId::from_fsid_and_inodeid(
                            SYSFS_ID,
                            get_inode_id(&[$path, $entry_name]),
                        ),
                        name: $entry_name.to_string(),
                        offset: (entries.len() + 1) as u64,
                        file_type: $entry_type,
                    });
                )*
                Ok(Box::new(SimpleDirStream::new(entries, start_offset)))
            }
        }
    };
}

static_dir! {
    RootInode,
    "",
}

pub struct CgroupFs {
    root: Arc<RootInode>,
}

impl CgroupFs {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            root: Arc::new(RootInode::new(InodeId::from_fsid_and_inodeid(
                CGROUPFS_ID,
                get_inode_id(&[]),
            ))),
        })
    }
}

#[async_trait]
impl Filesystem for CgroupFs {
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Ok(self.root.clone())
    }

    fn id(&self) -> u64 {
        CGROUPFS_ID
    }

    fn magic(&self) -> u64 {
        0x63677270 // v2 magic number
    }
}

static SYSFS_INSTANCE: OnceLock<Arc<CgroupFs>> = OnceLock::new();

/// Initializes and/or returns the global singleton [`CgroupFs`] instance.
/// This is the main entry point for the rest of the kernel to interact with cgroupfs.
pub fn cgroupfs() -> Arc<CgroupFs> {
    SYSFS_INSTANCE
        .get_or_init(|| {
            log::info!("cgroupfs initialized");
            CgroupFs::new()
        })
        .clone()
}

pub struct CgroupFsDriver;

impl CgroupFsDriver {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for CgroupFsDriver {
    fn name(&self) -> &'static str {
        "cgroupfs"
    }

    fn as_filesystem_driver(self: Arc<Self>) -> Option<Arc<dyn FilesystemDriver>> {
        Some(self)
    }
}

#[async_trait]
impl FilesystemDriver for CgroupFsDriver {
    async fn construct(
        &self,
        _fs_id: u64,
        device: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        if device.is_some() {
            warn!("cgroupfs should not be constructed with a block device");
            return Err(KernelError::InvalidValue);
        }
        Ok(cgroupfs())
    }
}
