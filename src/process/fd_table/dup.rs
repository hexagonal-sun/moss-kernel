use crate::sched::current_task;
use libkernel::{
    error::{KernelError, Result},
    fs::OpenFlags,
};

use super::{Fd, FdFlags, FileDescriptorEntry};

pub fn sys_dup(fd: Fd) -> Result<usize> {
    let task = current_task();
    let mut files = task.fd_table.lock_save_irq();

    let fd = files.get(fd).ok_or(KernelError::BadFd)?;

    let new_fd = files.insert(fd.clone())?;

    Ok(new_fd.as_raw() as _)
}

pub fn sys_dup3(oldfd: Fd, newfd: Fd, flags: u32) -> Result<usize> {
    if oldfd == newfd {
        return Err(KernelError::InvalidValue);
    }

    let flags = OpenFlags::from_bits_retain(flags);

    if !flags.difference(OpenFlags::O_CLOEXEC).is_empty() {
        // We only permit the O_CLOEXEC flag for dup3.
        return Err(KernelError::InvalidValue);
    }

    let task = current_task();
    let mut files = task.fd_table.lock_save_irq();

    let old_file = files.get(oldfd).ok_or(KernelError::BadFd)?;

    files.insert_at(
        newfd,
        FileDescriptorEntry {
            item: old_file.clone(),
            flags: if flags.contains(OpenFlags::O_CLOEXEC) {
                FdFlags::CLOEXEC
            } else {
                FdFlags::empty()
            },
        },
    );

    Ok(newfd.as_raw() as _)
}
