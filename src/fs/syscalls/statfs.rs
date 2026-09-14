use crate::fs::VFS;
use crate::memory::uaccess::cstr::UserCStr;
use crate::memory::uaccess::{UserCopyable, copy_to_user};
use crate::process::fd_table::Fd;
use crate::sched::syscall_ctx::ProcessCtx;
use alloc::sync::Arc;
use core::ffi::c_char;
use libkernel::error::{FsError, KernelError};
use libkernel::fs::Inode;
use libkernel::fs::path::Path;
use libkernel::fs::stats::FilesystemStats;
use libkernel::memory::address::TUA;
use libkernel::pod::Pod;

// `__statfs_word` is `__kernel_long_t` on AArch64. Keep this definition in
// sync with include/uapi/asm-generic/statfs.h rather than with the host ABI.
type FswordT = i64;
type FsBlockCntT = u64;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Fsid {
    val: [i32; 2],
}

impl From<u64> for Fsid {
    fn from(value: u64) -> Self {
        Self {
            val: [value as u32 as i32, (value >> 32) as u32 as i32],
        }
    }
}

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
    f_fsid: Fsid,
    /// Maximum length of filenames
    f_namelen: FswordT,
    /// Fragment size (since Linux 2.6)
    f_frsize: FswordT,
    /// Mount flags of filesystem (since Linux 2.6.36)
    f_flags: FswordT,
    /// Padding bytes reserved for future use
    f_spare: [FswordT; 4],
}

unsafe impl Pod for StatFs {}

unsafe impl UserCopyable for StatFs {}

async fn statfs_impl(inode: Arc<dyn Inode>) -> libkernel::error::Result<StatFs> {
    let fs = VFS.get_fs(inode).await?;
    Ok(fs.statfs().await?.into())
}

impl From<FilesystemStats> for StatFs {
    fn from(stats: FilesystemStats) -> Self {
        Self {
            f_type: stats.magic as _,
            f_bsize: stats.block_size as _,
            f_blocks: stats.blocks,
            f_bfree: stats.blocks_free,
            f_bavail: stats.blocks_available,
            f_files: stats.files,
            f_ffree: stats.files_free,
            f_fsid: stats.id.into(),
            f_namelen: stats.name_length as _,
            f_frsize: if stats.fragment_size == 0 {
                stats.block_size
            } else {
                stats.fragment_size
            } as _,
            f_flags: stats.flags as _,
            f_spare: [0; 4],
        }
    }
}

pub async fn sys_statfs(
    ctx: &ProcessCtx,
    path: TUA<c_char>,
    stat: TUA<StatFs>,
) -> libkernel::error::Result<usize> {
    let mut buf = [0; 1024];
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    if path.as_str().is_empty() {
        return Err(FsError::NotFound.into());
    }
    let task = ctx.shared().clone();
    let cwd = task.fs().cwd.lock_save_irq().clone();
    let inode = VFS.resolve_path(path, cwd, &task).await?;
    let statfs = statfs_impl(inode.inode()).await?;
    copy_to_user(stat, statfs).await?;
    Ok(0)
}

pub async fn sys_fstatfs(
    ctx: &ProcessCtx,
    fd: Fd,
    stat: TUA<StatFs>,
) -> libkernel::error::Result<usize> {
    let fd = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .get_raw(fd)
        .ok_or(KernelError::BadFd)?;
    let inode = fd.vfs_path().ok_or(KernelError::InvalidValue)?;
    let statfs = statfs_impl(inode.inode()).await?;
    copy_to_user(stat, statfs).await?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::StatFs;
    use core::mem::{align_of, offset_of, size_of};
    use libkernel::fs::stats::FilesystemStats;
    use moss_macros::ktest;

    #[ktest]
    fn statfs_matches_aarch64_uapi_layout() {
        assert_eq!(size_of::<StatFs>(), 120);
        assert_eq!(align_of::<StatFs>(), 8);
        assert_eq!(offset_of!(StatFs, f_type), 0);
        assert_eq!(offset_of!(StatFs, f_bsize), 8);
        assert_eq!(offset_of!(StatFs, f_blocks), 16);
        assert_eq!(offset_of!(StatFs, f_fsid), 56);
        assert_eq!(offset_of!(StatFs, f_namelen), 64);
        assert_eq!(offset_of!(StatFs, f_flags), 80);
        assert_eq!(offset_of!(StatFs, f_spare), 88);
    }

    #[ktest]
    fn statfs_converts_all_fields_and_clears_reserved_words() {
        let stats = FilesystemStats {
            magic: 0x5049_4446,
            block_size: 4096,
            blocks: 1,
            blocks_free: 2,
            blocks_available: 3,
            files: 4,
            files_free: 5,
            id: 0x89ab_cdef_0123_4567,
            name_length: 255,
            fragment_size: 512,
            flags: 0x20,
        };
        let abi = StatFs::from(stats);
        assert_eq!(abi.f_type, stats.magic as i64);
        assert_eq!(abi.f_bsize, 4096);
        assert_eq!((abi.f_blocks, abi.f_bfree, abi.f_bavail), (1, 2, 3));
        assert_eq!((abi.f_files, abi.f_ffree), (4, 5));
        assert_eq!(abi.f_fsid.val, [0x0123_4567, 0x89ab_cdefu32 as i32]);
        assert_eq!(abi.f_namelen, 255);
        assert_eq!(abi.f_frsize, 512);
        assert_eq!(abi.f_flags, 0x20);
        assert_eq!(abi.f_spare, [0; 4]);
        assert_eq!(
            StatFs::from(FilesystemStats {
                fragment_size: 0,
                ..stats
            })
            .f_frsize,
            4096
        );
    }
}
