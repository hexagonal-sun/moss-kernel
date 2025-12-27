use bitflags::Flags;
use libkernel::error::{KernelError, Result};

use crate::{process::fd_table::FdFlags, sched::current_task};

use super::{Fd, FileDescriptorEntryItem};

const F_DUPFD: u32 = 0; // Duplicate file descriptor.
const F_GETFD: u32 = 1; // Get file descriptor flags.
const F_SETFD: u32 = 2; // Set file descriptor flags.
const F_GETFL: u32 = 3; // Get file status flags.
const F_SETFL: u32 = 4; // Set file status flags.

pub async fn sys_fcntl(fd: Fd, op: u32, arg: usize) -> Result<usize> {
    let task = current_task();

    match op {
        F_DUPFD => todo!(),
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
                match fd.item.clone() {
                    FileDescriptorEntryItem::File(file) => file,
                    FileDescriptorEntryItem::Socket(_) => {
                        return Err(KernelError::InvalidValue);
                    }
                }
            };

            Ok(open_fd.flags().await.bits() as _)
        }
        F_SETFL => todo!(),
        _ => Err(KernelError::InvalidValue),
    }
}
