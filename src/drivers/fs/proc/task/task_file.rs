use crate::process::{Tid, find_task_by_tid};
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
    Maps,
    Exe,
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
            "maps" => Ok(TaskFileType::Maps),
            "exe" => Ok(TaskFileType::Exe),
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
                    | TaskFileType::Maps
                    | TaskFileType::Stat => FileType::File,
                    TaskFileType::Cwd | TaskFileType::Root | TaskFileType::Exe => FileType::Symlink,
                },
                permissions: FilePermissions::from_bits_retain(0o444),
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
        let task_details = find_task_by_tid(self.tid);

        let status_string = if let Some(task) = task_details {
            let state = task.state.load(core::sync::atomic::Ordering::Relaxed);
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
                    let vm = task.vm.lock_save_irq();

                    let mut vsize = 0;
                    let mut startcode = 0;
                    let mut endcode = 0;
                    let mut startstack = 0;
                    let mut start_data = 0;
                    let mut end_data = 0;

                    for vma in vm.mm().iter_vmas() {
                        let start = vma.region().start_address().value();
                        let end = vma.region().end_address().value();
                        vsize += vma.region().size();

                        if vma.name() == "[stack]" {
                            startstack = start;
                        } else if vma.permissions().execute {
                            if startcode == 0 || start < startcode {
                                startcode = start;
                            }
                            if end > endcode {
                                endcode = end;
                            }
                        } else if vma.permissions().write && !vma.name().starts_with('[') {
                            if start_data == 0 || start < start_data {
                                start_data = start;
                            }
                            if end > end_data {
                                end_data = end;
                            }
                        }
                    }

                    let start_brk = vm.start_brk().value();

                    let mut output = String::new();
                    output.push_str(&format!("{} ", task.process.tgid.value())); // pid
                    output.push_str(&format!("({}) ", name.as_str())); // comm
                    output.push_str(&format!("{state}")); // state
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
                    output.push_str(&format!("{} ", task.process.tasks.lock_save_irq().len())); // num_threads
                    output.push_str(&format!("{} ", 0)); // itrealvalue
                    output.push_str(&format!("{} ", 0)); // starttime
                    output.push_str(&format!("{vsize} ")); // vsize
                    output.push_str(&format!("{} ", 0)); // rss
                    output.push_str(&format!("{} ", 0)); // rsslim
                    output.push_str(&format!("{startcode} ")); // startcode
                    output.push_str(&format!("{endcode} ")); // endcode
                    output.push_str(&format!("{startstack} ")); // startstack
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
                    output.push_str(&format!(
                        "{} ",
                        task.sched_data
                            .lock_save_irq()
                            .as_ref()
                            .map_or(0, |s| s.last_cpu)
                    )); // processor
                    output.push_str(&format!("{} ", 0)); // rt_priority
                    output.push_str(&format!("{} ", 0)); // policy
                    output.push_str(&format!("{} ", 0)); // delayacct_blkio_ticks
                    output.push_str(&format!("{} ", 0)); // guest_time
                    output.push_str(&format!("{} ", 0)); // cguest_time
                    output.push_str(&format!("{start_data} ")); // start_data
                    output.push_str(&format!("{end_data} ")); // end_data
                    output.push_str(&format!("{start_brk} ")); // start_brk
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
                TaskFileType::Maps => {
                    let mut output = String::new();
                    let mut vm = task.vm.lock_save_irq();

                    for vma in vm.mm_mut().iter_vmas() {
                        output.push_str(&format!(
                            "{:x}-{:x} {}{}{}{} {:010x}                    {}\n",
                            vma.region().start_address().value(),
                            vma.region().end_address().value(),
                            if vma.permissions().read { "r" } else { "-" },
                            if vma.permissions().write { "w" } else { "-" },
                            if vma.permissions().execute { "x" } else { "-" },
                            "p", // Don't suport shared mappings... yet!
                            vma.file_offset().unwrap_or_default(),
                            vma.name()
                        ));
                    }

                    output
                }
                TaskFileType::Exe => {
                    if let Some(exe) = task.process.executable.lock_save_irq().clone() {
                        // TODO: Check if exists
                        exe.as_str().to_string()
                    } else {
                        "(deleted)".to_string()
                    }
                }
            }
        } else {
            "State:\tGone\n".to_string()
        };
        Ok(status_string.into_bytes())
    }

    async fn readlink(&self) -> libkernel::error::Result<PathBuf> {
        if let TaskFileType::Cwd = self.file_type {
            let task = find_task_by_tid(self.tid);
            return if let Some(task) = task {
                let cwd = task.cwd.lock_save_irq();
                Ok(cwd.1.clone())
            } else {
                Err(FsError::NotFound.into())
            };
        } else if let TaskFileType::Root = self.file_type {
            let task = find_task_by_tid(self.tid);
            return if let Some(task) = task {
                let root = task.root.lock_save_irq();
                Ok(root.1.clone())
            } else {
                Err(FsError::NotFound.into())
            };
        } else if let TaskFileType::Exe = self.file_type {
            let task_details = find_task_by_tid(self.tid);

            return if let Some(task) = task_details {
                if let Some(exe) = task.process.executable.lock_save_irq().clone() {
                    Ok(exe.as_str().to_string().into())
                } else {
                    Err(FsError::NotFound.into())
                }
            } else {
                Err(FsError::NotFound.into())
            };
        }
        Err(KernelError::NotSupported)
    }
}
