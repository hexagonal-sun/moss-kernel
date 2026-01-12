use crate::sched::current::current_task;
use libkernel::{
    error::{KernelError, Result},
    fs::OpenFlags,
};

use super::{Fd, FdFlags, FileDescriptorEntry};

pub fn dup_fd(fd: Fd, min_fd: Option<Fd>) -> Result<Fd> {
    let task = current_task();
    let mut files = task.fd_table.lock_save_irq();

    let file = files.get(fd).ok_or(KernelError::BadFd)?;

    let new_fd = match min_fd {
        Some(min_fd) => files.insert_above(min_fd, file.clone())?,
        None => files.insert(file.clone())?,
    };

    Ok(new_fd)
}

pub fn sys_dup(fd: Fd) -> Result<usize> {
    let new_fd = dup_fd(fd, None)?;

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
            file: old_file.clone(),
            flags: if flags.contains(OpenFlags::O_CLOEXEC) {
                FdFlags::CLOEXEC
            } else {
                FdFlags::empty()
            },
        },
    );

    Ok(newfd.as_raw() as _)
}
