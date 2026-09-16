//! Filesystem statistics, separate from the architecture-specific userspace ABI.

/// Linux marks the statfs mount-flags word as valid with this bit.
pub const ST_VALID: u64 = 0x20;

/// Statistics belonging to a filesystem instance, not to an individual inode.
#[derive(Debug, Clone, Copy, Default)]
pub struct FilesystemStats {
    /// Filesystem type magic.
    pub magic: u64,
    /// Optimal transfer block size in bytes.
    pub block_size: u64,
    /// Total data blocks.
    pub blocks: u64,
    /// Free data blocks.
    pub blocks_free: u64,
    /// Data blocks available to unprivileged users.
    pub blocks_available: u64,
    /// Total inodes.
    pub files: u64,
    /// Free inodes.
    pub files_free: u64,
    /// Unique filesystem instance ID.
    pub id: u64,
    /// Maximum filename length in bytes.
    pub name_length: u64,
    /// Allocation unit size in bytes; zero means use `block_size`.
    pub fragment_size: u64,
    /// Linux statfs flags for the filesystem's mount.
    pub flags: u64,
}
