use crate::drivers::fs::pidfs;
use crate::fs::fops::FileOps;
use crate::fs::open_file::OpenFile;
use crate::process::fd_table::FdFlags;
use crate::process::thread_group::pid::PidT;
use crate::process::{find_task_by_tid, pid_namespace::PidIdentity};
use crate::sched::syscall_ctx::ProcessCtx;
use alloc::boxed::Box;
use alloc::sync::Arc;
use async_trait::async_trait;
use bitflags::bitflags;
use libkernel::error::{KernelError, Result};
use libkernel::fs::OpenFlags;
use libkernel::memory::address::UA;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PidfdFlags: u32 {
        const PIDFD_NONBLOCK = OpenFlags::O_NONBLOCK.bits();
        const PIDFD_THREAD = OpenFlags::O_EXCL.bits();
    }
}

pub struct PidFile {
    _pid: Arc<PidIdentity>,
    _flags: PidfdFlags,
}

impl PidFile {
    pub fn new(pid: Arc<PidIdentity>, flags: PidfdFlags) -> Self {
        Self {
            _pid: pid,
            _flags: flags,
        }
    }

    pub fn new_open_file(pid: Arc<PidIdentity>, flags: PidfdFlags) -> Arc<OpenFile> {
        let file = PidFile::new(pid.clone(), flags);
        let mut open_file =
            OpenFile::new(Box::new(file), OpenFlags::from_bits(flags.bits()).unwrap());
        open_file.set_inode(pidfs::new_inode(pid));
        Arc::new(open_file)
    }
}

#[async_trait]
impl FileOps for PidFile {
    fn anonymous_name(&self) -> Option<&'static str> {
        Some("anon_inode:[pidfd]")
    }

    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }
}

pub async fn sys_pidfd_open(ctx: &ProcessCtx, pid: PidT, flags: u32) -> Result<usize> {
    if pid <= 0 {
        return Err(KernelError::InvalidValue);
    }
    let pid = ctx
        .shared()
        .pid_ns()
        .resolve(pid as u32)
        .ok_or(KernelError::NoProcess)?;
    let flags = PidfdFlags::from_bits(flags).ok_or(KernelError::InvalidValue)?;
    let task = find_task_by_tid(pid).ok_or(KernelError::NoProcess)?;
    if !flags.contains(PidfdFlags::PIDFD_THREAD) && task.process.tgid.value() != pid.value() {
        return Err(KernelError::NoProcess);
    }

    let file = PidFile::new_open_file(task.pid.clone(), flags);

    let fd = ctx
        .task()
        .fd_table
        .lock_save_irq()
        .insert_with_flags(file, FdFlags::CLOEXEC)?;

    Ok(fd.as_raw() as _)
}
