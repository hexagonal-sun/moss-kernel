use crate::{process::fd_table::Fd, sched::current_task};
use alloc::sync::Arc;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{FileType, Inode, path::Path},
};

pub mod access;
pub mod chmod;
pub mod chown;
pub mod link;
pub mod mkdir;
pub mod open;
pub mod readlink;
pub mod stat;
pub mod unlink;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct AtFlags: i32 {
        const AT_SYMLINK_NOFOLLOW = 0x100; // Do not follow symbolic links.
        const AT_EACCESS = 0x200;          // Check effective IDs in faccessat*.
        const AT_NO_AUTOMOUNT = 0x800;     // Suppress terminal automount traversal.
        const AT_EMPTY_PATH = 0x1000;      // Allow empty relative pathname.
    }
}

/// Given the paraters to one of the sys_{action}at syscalls, resolve the
/// arguments to a start node to which path should be applied.
async fn resolve_at_start_node(dirfd: Fd, path: &Path) -> Result<Arc<dyn Inode>> {
    let task = current_task();

    let start_node: Arc<dyn Inode> = if path.is_absolute() {
        // Absolute path ignores dirfd.
        task.root.lock_save_irq().0.clone()
    } else if dirfd.is_atcwd() {
        // Path is relative to the current working directory.
        task.cwd.lock_save_irq().0.clone()
    } else {
        // Path is relative to the directory specified by dirfd.
        let file = task
            .fd_table
            .lock_save_irq()
            .get(dirfd)
            .ok_or(KernelError::BadFd)?;

        let inode = file.inode().ok_or(KernelError::NotSupported)?;

        if inode.getattr().await?.file_type != FileType::Directory {
            return Err(FsError::NotADirectory.into());
        }

        inode
    };

    Ok(start_node)
}
