use core::ffi::c_char;

use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{
        attr::{AccessMode, FileAttr},
        path::Path,
    },
    memory::address::TUA,
    proc::caps::CapabilitiesFlags,
};
use ringbuf::Arc;

use crate::{
    clock::{realtime::date, timespec::TimeSpec},
    current_task,
    fs::syscalls::at::{AtFlags, resolve_at_start_node, resolve_path_flags},
    memory::uaccess::{copy_from_user, cstr::UserCStr},
    process::{Task, fd_table::Fd},
};

const UTIME_NOW: u64 = (1 << 30) - 1;
const UTIME_OMIT: u64 = (1 << 30) - 2;

pub async fn sys_utimensat(
    dirfd: Fd,
    path: TUA<c_char>,
    times: TUA<[TimeSpec; 2]>,
    flags: i32,
) -> Result<usize> {
    let task = current_task();

    // linux specifically uses NULL path to indicate futimens, see utimensat(2)
    let node = if path.is_null() {
        task.fd_table
            .lock_save_irq()
            .get(dirfd)
            .ok_or(KernelError::BadFd)?
            .inode()
            .ok_or(KernelError::BadFd)?
    } else {
        let mut buf = [0; 1024];

        let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
        let start_node = resolve_at_start_node(dirfd, path).await?;
        let flags = AtFlags::from_bits_retain(flags);

        resolve_path_flags(dirfd, path, start_node, task.clone(), flags).await?
    };

    let mut attr = node.getattr().await?;

    if times.is_null() {
        test_creds(task, &attr)?;
        attr.atime = date();
        attr.mtime = date();
        attr.ctime = date();
    } else {
        let times = copy_from_user(times).await?;
        if times[0].tv_nsec == UTIME_NOW && times[1].tv_nsec == UTIME_NOW {
            test_creds(task, &attr)?;
        } else if times[0].tv_nsec != UTIME_OMIT && times[1].tv_nsec != UTIME_OMIT {
            let creds = task.creds.lock_save_irq();
            if creds.euid() != attr.uid
                && !creds.caps().is_capable(CapabilitiesFlags::CAP_FOWNER)
                && !creds.caps().is_capable(CapabilitiesFlags::CAP_DAC_OVERRIDE)
            {
                return Err(FsError::PermissionDenied.into());
            }
        }

        let atime = match times[0].tv_nsec {
            UTIME_NOW => date(),
            UTIME_OMIT => attr.atime,
            _ => times[0].into(),
        };
        let mtime = match times[1].tv_nsec {
            UTIME_NOW => date(),
            UTIME_OMIT => attr.mtime,
            _ => times[1].into(),
        };

        attr.atime = atime;
        attr.mtime = mtime;
        attr.ctime = date();
    }

    node.setattr(attr).await?;

    Ok(0)
}

fn test_creds(task: Arc<Task>, attr: &FileAttr) -> Result<()> {
    let creds = task.creds.lock_save_irq();
    if attr
        .check_access(creds.uid(), creds.gid(), creds.caps(), AccessMode::W_OK)
        .is_err()
        && creds.euid() != attr.uid
        && !creds.caps().is_capable(CapabilitiesFlags::CAP_FOWNER)
        && !creds.caps().is_capable(CapabilitiesFlags::CAP_DAC_OVERRIDE)
    {
        Err(FsError::PermissionDenied.into())
    } else {
        Ok(())
    }
}
