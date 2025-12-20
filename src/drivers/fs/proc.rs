#![allow(clippy::module_name_repetitions)]

use crate::sched::{SCHED_STATE, current_task};
use crate::{
    drivers::{Driver, FilesystemDriver},
    process::TASK_LIST,
    sync::SpinLock,
};
use alloc::{boxed::Box, format, string::ToString, sync::Arc, vec::Vec};
use async_trait::async_trait;
use core::sync::atomic::{AtomicU64, Ordering};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{
        BlockDevice, DirStream, Dirent, FileType, Filesystem, Inode, InodeId, PROCFS_ID,
        attr::{FileAttr, FilePermissions},
    },
};
use log::warn;

pub struct ProcFs {
    root: Arc<ProcRootInode>,
    next_inode_id: AtomicU64,
}

impl ProcFs {
    fn new() -> Arc<Self> {
        let root_inode = Arc::new(ProcRootInode::new());
        Arc::new(Self {
            root: root_inode,
            next_inode_id: AtomicU64::new(1),
        })
    }

    /// Convenience helper to allocate a unique inode ID inside this filesystem.
    fn alloc_inode_id(&self) -> InodeId {
        let id = self.next_inode_id.fetch_add(1, Ordering::Relaxed);
        InodeId::from_fsid_and_inodeid(PROCFS_ID, id)
    }
}

#[async_trait]
impl Filesystem for ProcFs {
    async fn root_inode(&self) -> Result<Arc<dyn Inode>> {
        Ok(self.root.clone())
    }

    fn id(&self) -> u64 {
        PROCFS_ID
    }
}

struct ProcDirStream {
    entries: Vec<Dirent>,
    idx: usize,
}

#[async_trait]
impl DirStream for ProcDirStream {
    async fn next_entry(&mut self) -> Result<Option<Dirent>> {
        if self.idx < self.entries.len() {
            let ent = self.entries[self.idx].clone();
            self.idx += 1;
            Ok(Some(ent))
        } else {
            Ok(None)
        }
    }
}

struct ProcRootInode {
    id: InodeId,
    attr: SpinLock<FileAttr>,
}

impl ProcRootInode {
    fn new() -> Self {
        Self {
            id: InodeId::from_fsid_and_inodeid(PROCFS_ID, 0),
            attr: SpinLock::new(FileAttr {
                file_type: FileType::Directory,
                mode: FilePermissions::from_bits_retain(0o555),
                ..FileAttr::default()
            }),
        }
    }
}

#[async_trait]
impl Inode for ProcRootInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        // Lookup a PID directory.
        let pid: u32 = if name == "self" {
            let current_task = current_task();
            current_task.descriptor().tid().0
        } else {
            name.parse()
                .map_err(|_| FsError::NotFound)
                .map_err(Into::<KernelError>::into)?
        };

        // Validate that the process actually exists.
        if !TASK_LIST
            .lock_save_irq()
            .keys()
            .any(|d| d.tgid().value() == pid)
        {
            return Err(FsError::NotFound.into());
        }

        let fs = PROCFS_INSTANCE
            .lock_save_irq()
            .as_ref()
            .expect("ProcFS singleton not initialised")
            .clone();

        let inode_id = fs.alloc_inode_id();
        Ok(Arc::new(ProcTaskInode::new(pid, inode_id)))
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.lock_save_irq().clone())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let mut entries: Vec<Dirent> = Vec::new();
        // Gather task list under interrupt-safe lock.
        let task_list = TASK_LIST.lock_save_irq();
        for (idx, desc) in task_list.keys().enumerate() {
            // Use offset index as dirent offset.
            let name = desc.tgid().value().to_string();
            let inode_id = InodeId::from_fsid_and_inodeid(PROCFS_ID, (idx + 1) as u64);
            entries.push(Dirent::new(
                name,
                inode_id,
                FileType::Directory,
                (idx + 1) as u64,
            ));
        }
        entries.push(Dirent::new(
            "self".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, 0), // placeholder
            FileType::Directory,
            (entries.len() + 1) as u64,
        ));

        // honour start_offset
        let entries = if (start_offset as usize) < entries.len() {
            entries.into_iter().skip(start_offset as usize).collect()
        } else {
            Vec::new()
        };

        Ok(Box::new(ProcDirStream { entries, idx: 0 }))
    }
}

struct ProcTaskInode {
    id: InodeId,
    attr: SpinLock<FileAttr>,
    pid: u32,
}

impl ProcTaskInode {
    fn new(pid: u32, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: SpinLock::new(FileAttr {
                file_type: FileType::Directory,
                mode: FilePermissions::from_bits_retain(0o555),
                ..FileAttr::default()
            }),
            pid,
        }
    }
}

#[async_trait]
impl Inode for ProcTaskInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>> {
        if let Some(file_type) = TaskFileType::from_filename(name) {
            let fs = PROCFS_INSTANCE
                .lock_save_irq()
                .as_ref()
                .expect("ProcFS singleton not initialised")
                .clone();
            let inode_id = fs.alloc_inode_id();
            Ok(Arc::new(ProcTaskFileInode::new(
                self.pid, file_type, inode_id,
            )))
        } else {
            Err(FsError::NotFound.into())
        }
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.lock_save_irq().clone())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let mut entries: Vec<Dirent> = Vec::new();
        // Single entry: "status"
        let inode_id = InodeId::from_fsid_and_inodeid(PROCFS_ID, 0); // placeholder
        entries.push(Dirent::new(
            "status".to_string(),
            inode_id,
            FileType::File,
            1,
        ));
        entries.push(Dirent::new("comm".to_string(), inode_id, FileType::File, 2));
        entries.push(Dirent::new(
            "state".to_string(),
            inode_id,
            FileType::File,
            3,
        ));

        // honour start_offset
        let entries = if (start_offset as usize) < entries.len() {
            entries.into_iter().skip(start_offset as usize).collect()
        } else {
            Vec::new()
        };

        Ok(Box::new(ProcDirStream { entries, idx: 0 }))
    }
}

enum TaskFileType {
    Status,
    Comm,
    State,
}

impl TaskFileType {
    fn from_filename(name: &str) -> Option<Self> {
        match name {
            "status" => Some(TaskFileType::Status),
            "comm" => Some(TaskFileType::Comm),
            "state" => Some(TaskFileType::State),
            _ => None,
        }
    }
}

struct ProcTaskFileInode {
    id: InodeId,
    file_type: TaskFileType,
    attr: SpinLock<FileAttr>,
    pid: u32,
}

impl ProcTaskFileInode {
    fn new(pid: u32, file_type: TaskFileType, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: SpinLock::new(FileAttr {
                file_type: FileType::File,
                mode: FilePermissions::from_bits_retain(0o444),
                ..FileAttr::default()
            }),
            pid,
            file_type,
        }
    }
}

#[async_trait]
impl Inode for ProcTaskFileInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn lookup(&self, _name: &str) -> Result<Arc<dyn Inode>> {
        Err(FsError::NotADirectory.into())
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.lock_save_irq().clone())
    }

    async fn readdir(&self, _start_offset: u64) -> Result<Box<dyn DirStream>> {
        Err(FsError::NotADirectory.into())
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let pid = self.pid;
        let task_list = TASK_LIST.lock_save_irq();
        let id = task_list
            .iter()
            .find(|(desc, _)| desc.tgid().value() == pid);
        let task_details = if let Some((desc, _)) = id {
            SCHED_STATE.borrow().run_queue.get(desc).cloned()
        } else {
            None
        };

        let status_string = if let Some(task) = task_details {
            let state = *task.state.lock_save_irq();
            let name = task.comm.lock_save_irq();
            match self.file_type {
                TaskFileType::Status => format!(
                    "Name:\t{name}
State:\t{state}
Tgid:\t{tgid}
FDSize:\t{fd_size}
Pid:\t{pid}
Threads:\t{threads}\n",
                    name = name.as_str(),
                    tgid = task.process.tgid,
                    fd_size = task.fd_table.lock_save_irq().len(),
                    threads = task.process.threads.lock_save_irq().len(),
                ),
                TaskFileType::Comm => format!("{name}\n", name = name.as_str()),
                TaskFileType::State => format!("{state}\n"),
            }
        } else {
            "State:\tGone\n".to_string()
        };

        let bytes = status_string.as_bytes();
        let end = usize::min(bytes.len().saturating_sub(offset as usize), buf.len());
        if end == 0 {
            return Ok(0);
        }
        let slice = &bytes[offset as usize..offset as usize + end];
        buf[..end].copy_from_slice(slice);
        Ok(end)
    }
}

static PROCFS_INSTANCE: SpinLock<Option<Arc<ProcFs>>> = SpinLock::new(None);

pub struct ProcFsDriver;

impl ProcFsDriver {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Returns the global singleton instance of ProcFs, initialising it on the
    /// first call.
    fn fs(&self) -> Arc<ProcFs> {
        let mut guard = PROCFS_INSTANCE.lock_save_irq();
        guard.get_or_insert_with(ProcFs::new).clone()
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
        _fs_id: u64,
        device: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        if device.is_some() {
            warn!("procfs should not be constructed with a block device");
            return Err(KernelError::InvalidValue);
        }
        Ok(self.fs())
    }
}
