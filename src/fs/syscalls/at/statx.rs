use crate::{
    fs::syscalls::at::{resolve_at_start_node, resolve_path_flags},
    memory::uaccess::{UserCopyable, copy_to_user, cstr::UserCStr},
    process::fd_table::Fd,
    sched::syscall_ctx::ProcessCtx,
};
use core::{ffi::c_char, time::Duration};
use libkernel::{error::Result, fs::path::Path, memory::address::TUA};

use super::AtFlags;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct StatXMask: u32 {
        const STATX_TYPE = 0x0001;
        const STATX_MODE = 0x0002;
        const STATX_NLINK = 0x0004;
        const STATX_UID = 0x0008;
        const STATX_GID = 0x0010;
        const STATX_ATIME = 0x0020;
        const STATX_MTIME = 0x0040;
        const STATX_CTIME = 0x0080;
        const STATX_INO = 0x0100;
        const STATX_SIZE = 0x0200;
        const STATX_BLOCKS = 0x0400;
        const STATX_BASIC_STATS = 0x07ff;
        const STATX_BTIME = 0x0800;
        const STATX_ALL = 0x0fff;
        const STATX_MNT_ID = 0x1000;
        const STATX_DIOALIGN = 0x2000;
        const STATX_MNT_ID_UNIQUE = 0x4000;
        const STATX_SUBVOL = 0x8000;
        const STATX_WRITE_ATOMIC = 0x10000;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct StatXAttr: u64 {
        const STATX_ATTR_COMPRESSED = 0x0004;
        const STATX_ATTR_IMMUTABLE = 0x0010;
        const STATX_ATTR_APPEND = 0x0020;
        const STATX_ATTR_NODUMP = 0x0040;
        const STATX_ATTR_ENCRYPTED = 0x0800;
        const STATX_ATTR_AUTOMOUNT = 0x1000;
        const STATX_ATTR_MOUNT_ROOT = 0x2000;
        const STATX_ATTR_VERITY = 0x100000;
        const STATX_ATTR_DAX = 0x200000;
        const STATX_ATTR_NOSECURITY = 0x00400000;
    }
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct StatX {
    pub stx_mask: u32,             // Mask of supported fields
    pub stx_blksize: u32,          // Block size
    pub stx_attributes: u64,       // Attributes
    pub stx_nlink: u32,            // Link count
    pub stx_uid: u32,              // User ID of owner
    pub stx_gid: u32,              // Group ID of group
    pub stx_mode: u16,             // File mode
    pub __pad1: u16,               // Padding
    pub stx_ino: u64,              // Inode number
    pub stx_size: u64,             // Size
    pub stx_blocks: u64,           // Number of blocks allocated
    pub stx_attributes_mask: u64,  // Mask of supported attributes
    pub stx_atime: StatXTimestamp, // Access time
    pub stx_btime: StatXTimestamp, // Creation time
    pub stx_ctime: StatXTimestamp, // Change time
    pub stx_mtime: StatXTimestamp, // Modification time

    // Currently not supported on any current filesystems
    pub stx_rdev_major: u32,                // Device major ID
    pub stx_rdev_minor: u32,                // Device minor ID
    pub stx_dev_major: u32,                 // Filesystem major ID
    pub stx_dev_minor: u32,                 // Filesystem minor ID
    pub stx_mnt_id: u64,                    // Mount ID at Linux UAPI offset 0x90
    pub stx_dio_mem_align: u32,             // Alignment of memory for direct I/O
    pub stx_dio_offset_align: u32,          // Alignment of offset for direct I/O
    pub stx_subvol: u64,                    // Subvolume ID
    pub stx_atomic_write_unit_min: u32,     // Minimum atomic write direct I/O size
    pub stx_atomic_write_unit_max: u32,     // Maximum atomic write direct I/O size
    pub stx_atomic_write_segments_max: u32, // Maximum number of segments for atomic writes
    pub __spare1: u32,
    pub __spare3: [u64; 9],
}

// Validate the userspace layout in every kernel build.
const _: () = {
    assert!(core::mem::size_of::<StatX>() == 256);
    assert!(core::mem::offset_of!(StatX, stx_dev_major) == 136);
    assert!(core::mem::offset_of!(StatX, stx_mnt_id) == 144);
};

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct StatXTimestamp {
    pub tv_sec: i64,
    pub tv_nsec: u32,
    pub __pad1: i32,
}

impl From<Duration> for StatXTimestamp {
    fn from(duration: Duration) -> Self {
        Self {
            tv_sec: duration.as_secs() as i64,
            tv_nsec: duration.subsec_nanos(),
            __pad1: 0,
        }
    }
}

unsafe impl UserCopyable for StatX {}

pub async fn sys_statx(
    ctx: &ProcessCtx,
    dirfd: Fd,
    path: TUA<c_char>,
    flags: i32,
    mask: u32,
    statbuf: TUA<StatX>,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let task = ctx.shared().clone();
    let flags = AtFlags::from_bits_truncate(flags);
    let mask = StatXMask::from_bits_truncate(mask);
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);

    let start_node = resolve_at_start_node(ctx, dirfd, path, flags).await?;
    let node = resolve_path_flags(dirfd, path, start_node, &task, flags).await?;

    let attr = node.getattr().await?;

    // Decode the same Linux dev_t that ordinary stat returns.
    let dev = attr.id.fs_id();
    let mut stat_x = StatX {
        stx_blksize: attr.block_size,
        stx_dev_major: (((dev >> 8) & 0xfff) | ((dev >> 32) & 0xfffff000)) as u32,
        stx_dev_minor: ((dev & 0xff) | ((dev >> 12) & 0xffffff00)) as u32,
        ..StatX::default()
    };

    // TODO: right now, the attr is applied unconditionally even if the data is not supported, as
    // long as the input mask is set. at some point, the fs should be checked if these attributes
    // are supported and will return useful data.
    if mask.contains(StatXMask::STATX_TYPE) {
        stat_x.stx_mask |= StatXMask::STATX_TYPE.bits();
        stat_x.stx_mode = u32::from(attr.file_type) as u16;
    }

    if mask.contains(StatXMask::STATX_MODE) {
        stat_x.stx_mask |= StatXMask::STATX_MODE.bits();
        stat_x.stx_mode |= attr.mode().bits() as u16;
    }

    if mask.contains(StatXMask::STATX_NLINK) {
        stat_x.stx_mask |= StatXMask::STATX_NLINK.bits();
        stat_x.stx_nlink = attr.nlinks;
    }

    if mask.contains(StatXMask::STATX_UID) {
        stat_x.stx_mask |= StatXMask::STATX_UID.bits();
        stat_x.stx_uid = ctx
            .shared()
            .creds
            .lock_save_irq()
            .user_ns()
            .show_uid(attr.uid);
    }

    if mask.contains(StatXMask::STATX_GID) {
        stat_x.stx_mask |= StatXMask::STATX_GID.bits();
        stat_x.stx_gid = ctx
            .shared()
            .creds
            .lock_save_irq()
            .user_ns()
            .show_gid(attr.gid);
    }

    if mask.contains(StatXMask::STATX_ATIME) {
        stat_x.stx_mask |= StatXMask::STATX_ATIME.bits();
        stat_x.stx_atime = attr.atime.into();
    }

    if mask.contains(StatXMask::STATX_MTIME) {
        stat_x.stx_mask |= StatXMask::STATX_MTIME.bits();
        stat_x.stx_mtime = attr.mtime.into();
    }

    if mask.contains(StatXMask::STATX_CTIME) {
        stat_x.stx_mask |= StatXMask::STATX_CTIME.bits();
        stat_x.stx_ctime = attr.ctime.into();
    }

    if mask.contains(StatXMask::STATX_INO) {
        stat_x.stx_mask |= StatXMask::STATX_INO.bits();
        stat_x.stx_ino = attr.id.inode_id();
    }

    if mask.contains(StatXMask::STATX_SIZE) {
        stat_x.stx_mask |= StatXMask::STATX_SIZE.bits();
        stat_x.stx_size = attr.size;
    }

    if mask.contains(StatXMask::STATX_BLOCKS) {
        stat_x.stx_mask |= StatXMask::STATX_BLOCKS.bits();
        stat_x.stx_blocks = attr.blocks;
    }

    if mask.contains(StatXMask::STATX_BTIME) {
        stat_x.stx_mask |= StatXMask::STATX_BTIME.bits();
        stat_x.stx_btime = attr.btime.into();
    }

    if mask.contains(StatXMask::STATX_MNT_ID) {
        stat_x.stx_mask |= StatXMask::STATX_MNT_ID.bits();
        stat_x.stx_mnt_id = node.mount_id();
    }

    stat_x.stx_attributes_mask = StatXAttr::STATX_ATTR_MOUNT_ROOT.bits();
    if node.is_mount_root() {
        stat_x.stx_attributes |= StatXAttr::STATX_ATTR_MOUNT_ROOT.bits();
    }

    copy_to_user(statbuf, stat_x).await?;

    Ok(0)
}
