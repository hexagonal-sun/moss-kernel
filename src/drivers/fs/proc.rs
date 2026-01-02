#![allow(clippy::module_name_repetitions)]

use crate::process::find_task_by_descriptor;
use crate::process::thread_group::Tgid;
use crate::sched::current::current_task;
use crate::sync::OnceLock;
use crate::{
    drivers::{Driver, FilesystemDriver},
    process::TASK_LIST,
};
use alloc::string::String;
use alloc::{boxed::Box, format, string::ToString, sync::Arc, vec::Vec};
use async_trait::async_trait;
use core::sync::atomic::{AtomicU64, Ordering};
use libkernel::fs::pathbuf::PathBuf;
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
        Ok(if let Some(entry) = self.entries.get(self.idx).cloned() {
            self.idx += 1;
            Some(entry)
        } else {
            None
        })
    }
}

struct ProcRootInode {
    id: InodeId,
    attr: FileAttr,
}

impl ProcRootInode {
    fn new() -> Self {
        Self {
            id: InodeId::from_fsid_and_inodeid(PROCFS_ID, 0),
            attr: FileAttr {
                file_type: FileType::Directory,
                mode: FilePermissions::from_bits_retain(0o555),
                ..FileAttr::default()
            },
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
        let pid = if name == "self" {
            let current_task = current_task();
            current_task.descriptor().tgid()
        } else {
            Tgid(
                name.parse()
                    .map_err(|_| FsError::NotFound)
                    .map_err(Into::<KernelError>::into)?,
            )
        };

        // Validate that the process actually exists.
        if !TASK_LIST.lock_save_irq().keys().any(|d| d.tgid() == pid) {
            return Err(FsError::NotFound.into());
        }

        let fs = procfs();

        let inode_id = fs.alloc_inode_id();
        Ok(Arc::new(ProcTaskInode::new(pid, inode_id)))
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let mut entries: Vec<Dirent> = Vec::new();
        // Gather task list under interrupt-safe lock.
        let task_list = TASK_LIST.lock_save_irq();
        for (idx, desc) in task_list.keys().enumerate() {
            // Use offset index as dirent offset.
            let name = desc.tgid().value().to_string();
            let inode_id =
                InodeId::from_fsid_and_inodeid(PROCFS_ID, ((desc.tgid().0 + 1) * 100) as u64);
            entries.push(Dirent::new(
                name,
                inode_id,
                FileType::Directory,
                (idx + 1) as u64,
            ));
        }
        let current_task = current_task();
        entries.push(Dirent::new(
            "self".to_string(),
            InodeId::from_fsid_and_inodeid(
                PROCFS_ID,
                ((current_task.process.tgid.0 + 1) * 100) as u64,
            ),
            FileType::Directory,
            (entries.len() + 1) as u64,
        ));

        // honour start_offset
        let entries = entries.into_iter().skip(start_offset as usize).collect();

        Ok(Box::new(ProcDirStream { entries, idx: 0 }))
    }
}

struct ProcTaskInode {
    id: InodeId,
    attr: FileAttr,
    pid: Tgid,
}

impl ProcTaskInode {
    fn new(pid: Tgid, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: FileType::Directory,
                mode: FilePermissions::from_bits_retain(0o555),
                ..FileAttr::default()
            },
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
        if let Ok(file_type) = TaskFileType::try_from(name) {
            let fs = procfs();
            let inode_id = fs.alloc_inode_id();
            Ok(Arc::new(ProcTaskFileInode::new(
                self.pid, file_type, inode_id,
            )))
        } else {
            Err(FsError::NotFound.into())
        }
    }

    async fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn readdir(&self, start_offset: u64) -> Result<Box<dyn DirStream>> {
        let mut entries: Vec<Dirent> = Vec::new();
        let inode_offset = ((self.pid.value() + 1) * 100) as u64;
        entries.push(Dirent::new(
            "status".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, inode_offset + 1),
            FileType::File,
            1,
        ));
        entries.push(Dirent::new(
            "comm".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, inode_offset + 2),
            FileType::File,
            2,
        ));
        entries.push(Dirent::new(
            "state".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, inode_offset + 3),
            FileType::File,
            3,
        ));
        entries.push(Dirent::new(
            "cwd".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, inode_offset + 4),
            FileType::Symlink,
            4,
        ));
        entries.push(Dirent::new(
            "stat".to_string(),
            InodeId::from_fsid_and_inodeid(PROCFS_ID, inode_offset + 5),
            FileType::File,
            5,
        ));

        // honour start_offset
        let entries = entries.into_iter().skip(start_offset as usize).collect();

        Ok(Box::new(ProcDirStream { entries, idx: 0 }))
    }
}

enum TaskFileType {
    Status,
    Comm,
    Cwd,
    State,
    Stat,
}

impl TryFrom<&str> for TaskFileType {
    type Error = ();

    fn try_from(value: &str) -> core::result::Result<TaskFileType, Self::Error> {
        match value {
            "status" => Ok(TaskFileType::Status),
            "comm" => Ok(TaskFileType::Comm),
            "state" => Ok(TaskFileType::State),
            "stat" => Ok(TaskFileType::Stat),
            "cwd" => Ok(TaskFileType::Cwd),
            _ => Err(()),
        }
    }
}

struct ProcTaskFileInode {
    id: InodeId,
    file_type: TaskFileType,
    attr: FileAttr,
    pid: Tgid,
}

impl ProcTaskFileInode {
    fn new(pid: Tgid, file_type: TaskFileType, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: match file_type {
                    TaskFileType::Status
                    | TaskFileType::Comm
                    | TaskFileType::State
                    | TaskFileType::Stat => FileType::File,
                    TaskFileType::Cwd => FileType::Symlink,
                },
                mode: FilePermissions::from_bits_retain(0o444),
                ..FileAttr::default()
            },
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
        Ok(self.attr.clone())
    }

    async fn readdir(&self, _start_offset: u64) -> Result<Box<dyn DirStream>> {
        Err(FsError::NotADirectory.into())
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let pid = self.pid;
        // TODO: task needs to be the main thread of the process
        let task_list = TASK_LIST.lock_save_irq();
        let id = task_list
            .iter()
            .find(|(desc, _)| desc.tgid() == pid)
            .map(|(desc, _)| *desc);
        drop(task_list);
        let task_details = if let Some(desc) = id {
            find_task_by_descriptor(&desc)
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
Threads:\t{tasks}\n",
                    name = name.as_str(),
                    tgid = task.process.tgid,
                    fd_size = task.fd_table.lock_save_irq().len(),
                    tasks = task.process.tasks.lock_save_irq().len(),
                ),
                TaskFileType::Comm => format!("{name}\n", name = name.as_str()),
                TaskFileType::State => format!("{state}\n"),
                TaskFileType::Stat => {
                    let mut output = String::new();
                    output.push_str(&format!("{} ", task.process.tgid.0)); // pid
                    output.push_str(&format!("({}) ", name.as_str())); // comm
                    output.push_str(&format!("{} ", state)); // state
                    output.push_str(&format!("{} ", 0)); // ppid
                    output.push_str(&format!("{} ", 0)); // pgrp
                    output.push_str(&format!("{} ", task.process.sid.lock_save_irq().value())); // session
                    output.push_str(&format!("{} ", 0)); // tty_nr
                    output.push_str(&format!("{} ", 0)); // tpgid
                    output.push_str(&format!("{} ", 0)); // flags
                    output.push_str(&format!("{} ", 0)); // minflt
                    output.push_str(&format!("{} ", 0)); // cminflt
                    output.push_str(&format!("{} ", 0)); // majflt
                    output.push_str(&format!("{} ", 0)); // cmajflt
                    output.push_str(&format!("{} ", task.process.utime.load(Ordering::Relaxed))); // utime
                    output.push_str(&format!("{} ", task.process.stime.load(Ordering::Relaxed))); // stime
                    output.push_str(&format!("{} ", 0)); // cutime
                    output.push_str(&format!("{} ", 0)); // cstime
                    output.push_str(&format!("{} ", *task.process.priority.lock_save_irq())); // priority
                    output.push_str(&format!("{} ", 0)); // nice
                    output.push_str(&format!("{} ", 0)); // num_threads
                    output.push_str(&format!("{} ", 0)); // itrealvalue
                    output.push_str(&format!("{} ", 0)); // starttime
                    output.push_str(&format!("{} ", 0)); // vsize
                    output.push_str(&format!("{} ", 0)); // rss
                    output.push_str(&format!("{} ", 0)); // rsslim
                    output.push_str(&format!("{} ", 0)); // startcode
                    output.push_str(&format!("{} ", 0)); // endcode
                    output.push_str(&format!("{} ", 0)); // startstack
                    output.push_str(&format!("{} ", 0)); // kstkesp
                    output.push_str(&format!("{} ", 0)); // kstkeip
                    output.push_str(&format!("{} ", 0)); // signal
                    output.push_str(&format!("{} ", 0)); // blocked
                    output.push_str(&format!("{} ", 0)); // sigignore
                    output.push_str(&format!("{} ", 0)); // sigcatch
                    output.push_str(&format!("{} ", 0)); // wchan
                    output.push_str(&format!("{} ", 0)); // nswap
                    output.push_str(&format!("{} ", 0)); // cnswap
                    output.push_str(&format!("{} ", 0)); // exit_signal
                    output.push_str(&format!("{} ", 0)); // processor
                    output.push_str(&format!("{} ", 0)); // rt_priority
                    output.push_str(&format!("{} ", 0)); // policy
                    output.push_str(&format!("{} ", 0)); // delayacct_blkio_ticks
                    output.push_str(&format!("{} ", 0)); // guest_time
                    output.push_str(&format!("{} ", 0)); // cguest_time
                    output.push_str(&format!("{} ", 0)); // start_data
                    output.push_str(&format!("{} ", 0)); // end_data
                    output.push_str(&format!("{} ", 0)); // start_brk
                    output.push_str(&format!("{} ", 0)); // arg_start
                    output.push_str(&format!("{} ", 0)); // arg_end
                    output.push_str(&format!("{} ", 0)); // env_start
                    output.push_str(&format!("{} ", 0)); // env_end
                    output.push_str(&format!("{} ", 0)); // exit_code
                    output.push('\n');
                    output
                }
                TaskFileType::Cwd => task.cwd.lock_save_irq().clone().1.as_str().to_string(),
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

    async fn readlink(&self) -> Result<PathBuf> {
        if let TaskFileType::Cwd = self.file_type {
            let pid = self.pid;
            let task_list = TASK_LIST.lock_save_irq();
            let id = task_list.iter().find(|(desc, _)| desc.tgid() == pid);
            let task_details = if let Some((desc, _)) = id {
                find_task_by_descriptor(desc)
            } else {
                None
            };
            return if let Some(task) = task_details {
                let cwd = task.cwd.lock_save_irq();
                Ok(cwd.1.clone())
            } else {
                Err(FsError::NotFound.into())
            };
        }
        Err(KernelError::NotSupported)
    }
}

static PROCFS_INSTANCE: OnceLock<Arc<ProcFs>> = OnceLock::new();

/// Initializes and/or returns the global singleton [`ProcFs`] instance.
/// This is the main entry point for the rest of the kernel to interact with procfs.
pub fn procfs() -> Arc<ProcFs> {
    PROCFS_INSTANCE
        .get_or_init(|| {
            log::info!("procfs initialized");
            ProcFs::new()
        })
        .clone()
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
        _fs_id: u64,
        device: Option<Box<dyn BlockDevice>>,
    ) -> Result<Arc<dyn Filesystem>> {
        if device.is_some() {
            warn!("procfs should not be constructed with a block device");
            return Err(KernelError::InvalidValue);
        }
        Ok(procfs())
    }
}
