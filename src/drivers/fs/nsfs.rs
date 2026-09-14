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

#[derive(Clone)]
pub enum Namespace {
    User(Arc<UserNamespace>),
    Mount(Arc<crate::fs::mount::MountNamespace>),
    Pid(Arc<crate::process::pid_namespace::PidNamespace>),
}
impl Namespace {
    pub fn id(&self) -> u64 {
        match self {
            Self::User(n) => n.id,
            Self::Mount(n) => n.id,
            Self::Pid(n) => n.id,
        }
    }
    pub fn kind(&self) -> u32 {
        match self {
            Self::User(_) => CloneFlags::CLONE_NEWUSER.bits(),
            Self::Mount(_) => CloneFlags::CLONE_NEWNS.bits(),
            Self::Pid(_) => CloneFlags::CLONE_NEWPID.bits(),
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Mount(_) => "mnt",
            Self::Pid(_) => "pid",
        }
    }
}
pub struct NamespaceInode {
    pub ns: Namespace,
}
pub fn namespace_inode(ns: Namespace) -> Arc<dyn Inode> {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| VFS.register_internal_fs(Arc::new(NsFs)));
    Arc::new(NamespaceInode { ns })
}

#[async_trait]
impl Inode for NamespaceInode {
    fn id(&self) -> InodeId {
        InodeId::from_fsid_and_inodeid(NSFS_ID, self.ns.id())
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
    inode
        .as_any()
        .downcast_ref::<NamespaceInode>()
        .map(|inode| {
            Box::new(NsFile {
                ns: inode.ns.clone(),
            }) as Box<dyn FileOps>
        })
}
fn install_fd(ns: Namespace) -> Result<usize> {
    let mut file = OpenFile::new(Box::new(NsFile { ns: ns.clone() }), OpenFlags::O_RDONLY);
    file.set_inode(namespace_inode(ns));
    Ok(crate::sched::current_work()
        .task
        .fd_table
        .lock_save_irq()
        .insert_with_flags(Arc::new(file), FdFlags::CLOEXEC)?
        .as_raw() as usize)
}

struct NsFile {
    ns: Namespace,
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
            0xb702 if matches!(self.ns, Namespace::Pid(_)) => {
                let Namespace::Pid(ns) = &self.ns else {
                    unreachable!()
                };
                let parent = ns.parent.clone().ok_or(KernelError::NotPermitted)?;
                if !parent.within(&crate::sched::current_work().pid_ns()) {
                    return Err(KernelError::NotPermitted);
                }
                install_fd(Namespace::Pid(parent))
            }
            0xb701 | 0xb702 => {
                let parent = match &self.ns {
                    Namespace::User(ns) => ns.parent.clone().ok_or(KernelError::NotPermitted)?,
                    Namespace::Mount(ns) if request == 0xb701 => ns.owner.clone(),
                    Namespace::Mount(_) => return Err(KernelError::InvalidValue),
                    Namespace::Pid(ns) => ns.owner.clone(),
                };
                let mut ns = parent.as_ref();
                while ns != creds.user_ns().as_ref() {
                    ns = ns.parent.as_deref().ok_or(KernelError::NotPermitted)?;
                }
                install_fd(Namespace::User(parent))
            }
            0xb703 => Ok(self.ns.kind() as usize),
            0xb704 => {
                let Namespace::User(ns) = &self.ns else {
                    return Err(KernelError::InvalidValue);
                };
                copy_to_user(TUA::from_value(arg), creds.user_ns().show_uid(ns.owner)).await?;
                Ok(0)
            }
            _ => Err(KernelError::NotATty),
        }
    }
}
