use crate::{
    current_task,
    fs::syscalls::at::{resolve_at_start_node, resolve_path_flags},
    memory::uaccess::{UserCopyable, copy_to_user, cstr::UserCStr},
    process::fd_table::Fd,
};
use core::ffi::c_char;
use libkernel::{
    error::Result,
    fs::{attr::FileAttr, path::Path},
    memory::address::TUA,
};

use super::AtFlags;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub st_dev: u64,        // Device
    pub st_ino: u64,        // File serial number
    pub st_mode: u32,       // File mode
    pub st_nlink: u32,      // Link count
    pub st_uid: u32,        // User ID of owner
    pub st_gid: u32,        // Group ID of group
    pub st_rdev: u64,       // Device number (if special file)
    pub __pad1: u64,        // Padding
    pub st_size: i64,       // Size of file, in bytes
    pub st_blksize: i32,    // Optimal block size for I/O
    pub __pad2: i32,        // Padding
    pub st_blocks: i64,     // Number of 512B blocks allocated
    pub st_atime: i64,      // Time of last access
    pub st_atime_nsec: u64, // Nanoseconds of last access
    pub st_mtime: i64,      // Time of last modification
    pub st_mtime_nsec: u64, // Nanoseconds of last modification
    pub st_ctime: i64,      // Time of last status change
    pub st_ctime_nsec: u64, // Nanoseconds of last status change
    pub __unused4: u32,     // Unused
    pub __unused5: u32,     // Unused
}

unsafe impl UserCopyable for Stat {}

impl From<FileAttr> for Stat {
    fn from(value: FileAttr) -> Self {
        Self {
            st_dev: value.id.fs_id(),
            st_ino: value.id.inode_id(),
            st_mode: value.mode.bits() as u32 | u32::from(value.file_type),
            st_nlink: value.nlinks,
            st_uid: value.uid.into(),
            st_gid: value.gid.into(),
            st_rdev: 0,
            __pad1: 0,
            st_size: value.size as _,
            st_blksize: value.block_size as _,
            __pad2: 0,
            st_blocks: 0,
            st_atime: value.atime.as_secs() as _,
            st_atime_nsec: value.atime.subsec_nanos() as _,
            st_mtime: value.mtime.as_secs() as _,
            st_mtime_nsec: value.mtime.subsec_nanos() as _,
            st_ctime: value.ctime.as_secs() as _,
            st_ctime_nsec: value.ctime.subsec_nanos() as _,
            __unused4: 0,
            __unused5: 0,
        }
    }
}

pub async fn sys_newfstatat(
    dirfd: Fd,
    path: TUA<c_char>,
    statbuf: TUA<Stat>,
    flags: i32,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let task = current_task();
    let flags = AtFlags::from_bits_truncate(flags);
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);

    let start_node = resolve_at_start_node(dirfd, path).await?;
    let node = resolve_path_flags(dirfd, path, start_node, task.clone(), flags).await?;

    let attr = node.getattr().await?;

    copy_to_user(statbuf, attr.into()).await?;

    Ok(0)
}
