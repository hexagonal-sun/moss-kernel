use crate::fs::fops::FileOps;
use crate::fs::open_file::OpenFile;
use crate::process::thread_group::Tgid;
use crate::process::{TaskDescriptor, Tid, find_task_by_descriptor};
use crate::sched::current::current_task_shared;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::any::Any;
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
    pid: Tid,
    flags: PidfdFlags,
}

impl PidFile {
    pub fn new(pid: Tid, flags: PidfdFlags) -> Self {
        Self { pid, flags }
    }

    pub fn new_open_file(pid: Tid, flags: PidfdFlags) -> Arc<OpenFile> {
        let file = PidFile::new(pid, flags);
        Arc::new(OpenFile::new(
            Box::new(file),
            OpenFlags::from_bits(flags.bits()).unwrap(),
        ))
    }
}

#[async_trait]
impl FileOps for PidFile {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }
}

pub async fn sys_pidfd_open(pid: Tid, flags: u32) -> Result<usize> {
    let flags = PidfdFlags::from_bits(flags).ok_or(KernelError::InvalidValue)?;
    if !flags.contains(PidfdFlags::PIDFD_THREAD) {
        // Ensure the pid exists and is a thread group leader.
        let _ = find_task_by_descriptor(&TaskDescriptor::from_tgid_tid(Tgid(pid.value()), pid))
            .unwrap();
    }
    let task = current_task_shared();

    let file = PidFile::new_open_file(pid, flags);

    let fd = task.fd_table.lock_save_irq().insert(file)?;

    Ok(fd.as_raw() as _)
}
