use bitflags::Flags;
use libkernel::error::{KernelError, Result};

use super::Fd;
use crate::process::fd_table::dup::dup_fd;
use crate::{process::fd_table::FdFlags, sched::current::current_task_shared};

const F_DUPFD: u32 = 0; // Duplicate file descriptor.
const F_GETFD: u32 = 1; // Get file descriptor flags.
const F_SETFD: u32 = 2; // Set file descriptor flags.
const F_GETFL: u32 = 3; // Get file status flags.
const F_SETFL: u32 = 4; // Set file status flags.

pub async fn sys_fcntl(fd: Fd, op: u32, arg: usize) -> Result<usize> {
    let task = current_task_shared();

    match op {
        F_DUPFD => dup_fd(fd, Some(Fd(arg as i32))).map(|new_fd| new_fd.as_raw() as _),
        F_GETFD => {
            let fds = task.fd_table.lock_save_irq();
            let fd = fds
                .entries
                .get(fd.as_raw() as usize)
                .and_then(|entry| entry.as_ref())
                .ok_or(KernelError::BadFd)?;
            Ok(fd.flags.bits() as _)
        }
        F_SETFD => {
            let mut fds = task.fd_table.lock_save_irq();
            let fd = fds
                .entries
                .get_mut(fd.as_raw() as usize)
                .and_then(|entry| entry.as_mut())
                .ok_or(KernelError::BadFd)?;

            let new_flags = FdFlags::from_bits_retain(arg as _);
            if new_flags.contains_unknown_bits() {
                return Err(KernelError::InvalidValue);
            }
            fd.flags = new_flags;
            Ok(0)
        }
        F_GETFL => {
            let open_fd = {
                let mut fds = task.fd_table.lock_save_irq();
                let fd = fds
                    .entries
                    .get_mut(fd.as_raw() as usize)
                    .and_then(|entry| entry.as_mut())
                    .ok_or(KernelError::BadFd)?;

                fd.file.clone()
            };

            Ok(open_fd.flags().await.bits() as _)
        }
        F_SETFL => todo!(),
        _ => Err(KernelError::InvalidValue),
    }
}
