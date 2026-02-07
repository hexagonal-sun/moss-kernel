use crate::fs::VFS;
use crate::memory::uaccess::cstr::UserCStr;
use crate::memory::uaccess::{UserCopyable, copy_to_user};
use crate::process::fd_table::Fd;
use crate::sched::current::{current_task, current_task_shared};
use alloc::sync::Arc;
use core::ffi::c_char;
use libkernel::error::KernelError;
use libkernel::fs::Inode;
use libkernel::fs::path::Path;
use libkernel::memory::address::TUA;
use libkernel::pod::Pod;

type FswordT = u32;
type FsBlockCntT = u64;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StatFs {
    /// Type of filesystem
    f_type: FswordT,
    /// Optimal transfer block size
    f_bsize: FswordT,
    /// Total data blocks in filesystem
    f_blocks: FsBlockCntT,
    /// Free blocks in filesystem
    f_bfree: FsBlockCntT,
    /// Free blocks available to unprivileged user
    f_bavail: FsBlockCntT,
    /// Total inodes in filesystem
    f_files: FsBlockCntT,
    /// Free inodes in filesystem
    f_ffree: FsBlockCntT,
    /// Filesystem ID
    f_fsid: u64,
    /// Maximum length of filenames
    f_namelen: FswordT,
    /// Fragment size (since Linux 2.6)
    f_frsize: FswordT,
    /// Mount flags of filesystem (since Linux 2.6.36)
    f_flags: FswordT,
    /// Padding bytes reserved for future use
    f_spare: [FswordT; 6],
}

unsafe impl Pod for StatFs {}

unsafe impl UserCopyable for StatFs {}

async fn statfs_impl(inode: Arc<dyn Inode>) -> libkernel::error::Result<StatFs> {
    let fs = VFS.get_fs(inode).await?;
    Ok(StatFs {
        f_type: fs.magic() as _,
        f_bsize: 0,
        f_blocks: 0,
        f_bfree: 0,
        f_bavail: 0,
        f_files: 0,
        f_ffree: 0,
        f_fsid: fs.id(),
        f_namelen: 0,
        f_frsize: 0,
        f_flags: 0,
        f_spare: [0; 6],
    })
}

pub async fn sys_statfs(path: TUA<c_char>, stat: TUA<StatFs>) -> libkernel::error::Result<usize> {
    let mut buf = [0; 1024];
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let inode = VFS
        .resolve_path(path, VFS.root_inode(), &current_task_shared())
        .await?;
    let statfs = statfs_impl(inode).await?;
    copy_to_user(stat, statfs).await?;
    Ok(0)
}

pub async fn sys_fstatfs(fd: Fd, stat: TUA<StatFs>) -> libkernel::error::Result<usize> {
    let fd = current_task()
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;
    let statfs = statfs_impl(fd.inode().unwrap()).await?;
    copy_to_user(stat, statfs).await?;
    Ok(0)
}
