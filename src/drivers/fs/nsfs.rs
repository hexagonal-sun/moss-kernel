//! Namespace handles are actual nsfs inodes, not procfs path strings. An fd
//! owns an Arc to its namespace and remains useful after the target exits.
use crate::{
    fs::{
        VFS,
        fops::FileOps,
        open_file::{FileCtx, OpenFile},
    },
    memory::uaccess::copy_to_user,
    process::{clone::CloneFlags, fd_table::FdFlags, user_namespace::UserNamespace},
    sync::OnceLock,
};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use libkernel::{
    error::{KernelError, Result},
    fs::{
        Filesystem, Inode, InodeId, NSFS_ID, OpenFlags,
        attr::{FileAttr, FilePermissions},
        stats::{FilesystemStats, ST_VALID},
    },
    memory::{
        PAGE_SIZE,
        address::{TUA, UA},
    },
};

struct NsFs;
#[async_trait]
impl Filesystem for NsFs {
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Err(KernelError::NotSupported)
    }
    fn id(&self) -> u64 {
        NSFS_ID
    }
    fn magic(&self) -> u64 {
        0x6e73_6673
    }
    async fn statfs(&self) -> Result<FilesystemStats> {
        Ok(FilesystemStats {
            magic: self.magic(),
            id: self.id(),
            block_size: PAGE_SIZE as u64,
            name_length: 255,
            flags: ST_VALID,
            ..Default::default()
        })
    }
}

pub struct UserNsInode {
    pub ns: Arc<UserNamespace>,
}
pub fn new_inode(ns: Arc<UserNamespace>) -> Arc<dyn Inode> {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| VFS.register_internal_fs(Arc::new(NsFs)));
    Arc::new(UserNsInode { ns })
}

#[async_trait]
impl Inode for UserNsInode {
    fn id(&self) -> InodeId {
        InodeId::from_fsid_and_inodeid(NSFS_ID, self.ns.id)
    }
    async fn getattr(&self) -> Result<FileAttr> {
        Ok(FileAttr {
            id: self.id(),
            block_size: PAGE_SIZE as u32,
            permissions: FilePermissions::from_bits_retain(0o444),
            ..Default::default()
        })
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

pub fn open(inode: &dyn Inode) -> Option<Box<dyn FileOps>> {
    inode.as_any().downcast_ref::<UserNsInode>().map(|inode| {
        Box::new(NsFile {
            ns: inode.ns.clone(),
        }) as Box<dyn FileOps>
    })
}
fn install_fd(ns: Arc<UserNamespace>) -> Result<usize> {
    let mut file = OpenFile::new(Box::new(NsFile { ns: ns.clone() }), OpenFlags::O_RDONLY);
    file.set_inode(new_inode(ns));
    Ok(crate::sched::current_work()
        .task
        .fd_table
        .lock_save_irq()
        .insert_with_flags(Arc::new(file), FdFlags::CLOEXEC)?
        .as_raw() as usize)
}

struct NsFile {
    ns: Arc<UserNamespace>,
}
#[async_trait]
impl FileOps for NsFile {
    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }
    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }
    async fn ioctl(&mut self, _ctx: &mut FileCtx, request: usize, arg: usize) -> Result<usize> {
        let creds = crate::sched::current_work()
            .task
            .creds
            .lock_save_irq()
            .clone();
        match request {
            0xb701 | 0xb702 => {
                let parent = self.ns.parent.as_ref().ok_or(KernelError::NotPermitted)?;
                let mut ns = parent.as_ref();
                while ns != creds.user_ns().as_ref() {
                    ns = ns.parent.as_deref().ok_or(KernelError::NotPermitted)?;
                }
                install_fd(parent.clone())
            }
            0xb703 => Ok(CloneFlags::CLONE_NEWUSER.bits() as usize),
            0xb704 => {
                copy_to_user(
                    TUA::from_value(arg),
                    creds.user_ns().show_uid(self.ns.owner),
                )
                .await?;
                Ok(0)
            }
            _ => Err(KernelError::NotATty),
        }
    }
}
