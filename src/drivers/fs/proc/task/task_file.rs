use crate::process::{TASK_LIST, Tid, find_task_by_descriptor};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use async_trait::async_trait;
use core::sync::atomic::Ordering;
use libkernel::error::{FsError, KernelError};
use libkernel::fs::attr::{FileAttr, FilePermissions};
use libkernel::fs::pathbuf::PathBuf;
use libkernel::fs::{FileType, InodeId, SimpleFile};

pub enum TaskFileType {
    Status,
    Comm,
    Cwd,
    Root,
    State,
    Stat,
}

impl TryFrom<&str> for TaskFileType {
    type Error = ();

    fn try_from(value: &str) -> Result<TaskFileType, Self::Error> {
        match value {
            "status" => Ok(TaskFileType::Status),
            "comm" => Ok(TaskFileType::Comm),
            "state" => Ok(TaskFileType::State),
            "stat" => Ok(TaskFileType::Stat),
            "cwd" => Ok(TaskFileType::Cwd),
            "root" => Ok(TaskFileType::Root),
            _ => Err(()),
        }
    }
}

pub struct ProcTaskFileInode {
    id: InodeId,
    file_type: TaskFileType,
    attr: FileAttr,
    tid: Tid,
    process_stats: bool,
}

impl ProcTaskFileInode {
    pub fn new(tid: Tid, file_type: TaskFileType, process_stats: bool, inode_id: InodeId) -> Self {
        Self {
            id: inode_id,
            attr: FileAttr {
                file_type: match file_type {
                    TaskFileType::Status
                    | TaskFileType::Comm
                    | TaskFileType::State
                    | TaskFileType::Stat => FileType::File,
                    TaskFileType::Cwd | TaskFileType::Root => FileType::Symlink,
                },
                mode: FilePermissions::from_bits_retain(0o444),
                ..FileAttr::default()
            },
            process_stats,
            tid,
            file_type,
        }
    }
}

#[async_trait]
impl SimpleFile for ProcTaskFileInode {
    fn id(&self) -> InodeId {
        self.id
    }

    async fn getattr(&self) -> libkernel::error::Result<FileAttr> {
        Ok(self.attr.clone())
    }

    async fn read(&self) -> libkernel::error::Result<Vec<u8>> {
        let tid = self.tid;
        let task_list = TASK_LIST.lock_save_irq();
        let id = task_list
            .iter()
            .find(|(desc, _)| desc.tid() == tid)
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
                    pid = task.tid.value(),
                    tasks = task.process.tasks.lock_save_irq().len(),
                ),
                TaskFileType::Comm => format!("{name}\n", name = name.as_str()),
                TaskFileType::State => format!("{state}\n"),
                TaskFileType::Stat => {
                    let mut output = String::new();
                    output.push_str(&format!("{} ", task.process.tgid.value())); // pid
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
                    if self.process_stats {
                        output
                            .push_str(&format!("{} ", task.process.utime.load(Ordering::Relaxed))); // utime
                        output
                            .push_str(&format!("{} ", task.process.stime.load(Ordering::Relaxed))); // stime
                    } else {
                        output.push_str(&format!("{} ", task.utime.load(Ordering::Relaxed))); // utime
                        output.push_str(&format!("{} ", task.stime.load(Ordering::Relaxed))); // stime
                    }
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
                TaskFileType::Root => task.root.lock_save_irq().1.as_str().to_string(),
            }
        } else {
            "State:\tGone\n".to_string()
        };
        Ok(status_string.into_bytes())
    }

    async fn readlink(&self) -> libkernel::error::Result<PathBuf> {
        if let TaskFileType::Cwd = self.file_type {
            let tid = self.tid;
            let task_list = TASK_LIST.lock_save_irq();
            let id = task_list
                .iter()
                .find(|(desc, _)| desc.tid() == tid)
                .map(|(desc, _)| *desc);
            drop(task_list);
            let task_details = if let Some(desc) = id {
                find_task_by_descriptor(&desc)
            } else {
                None
            };
            return if let Some(task) = task_details {
                let cwd = task.cwd.lock_save_irq();
                Ok(cwd.1.clone())
            } else {
                Err(FsError::NotFound.into())
            };
        } else if let TaskFileType::Root = self.file_type {
            let tid = self.tid;
            let task_list = TASK_LIST.lock_save_irq();
            let id = task_list
                .iter()
                .find(|(desc, _)| desc.tid() == tid)
                .map(|(desc, _)| *desc);
            drop(task_list);
            let task_details = if let Some(desc) = id {
                find_task_by_descriptor(&desc)
            } else {
                None
            };
            return if let Some(task) = task_details {
                let root = task.root.lock_save_irq();
                Ok(root.1.clone())
            } else {
                Err(FsError::NotFound.into())
            };
        }
        Err(KernelError::NotSupported)
    }
}
