// From: https://github.com/torvalds/linux/blob/8f0b4cce4481fb22653697cced8d0d04027cb1e8/fs/ext4/ext4.h#L1345
use crate::pod::Pod;
#[repr(C)]
pub struct Ext4SuperBlock {
    /*00*/
    /// Inodes count
    pub inodes_count: u32,
    /// Blocks count
    pub blocks_count_lo: u32,
    /// Reserved blocks count
    pub r_blocks_count_lo: u32,
    /// Free blocks count
    pub free_blocks_count_lo: u32,
    /*10*/
    /// Free inodes count
    pub free_inodes_count: u32,
    /// First Data Block
    pub first_data_block: u32,
    /// Block size
    pub log_block_size: u32,
    /// Allocation cluster size
    pub log_cluster_size: u32,
    /*20*/
    /// \# Blocks per group
    pub blocks_per_group: u32,
    /// \# Clusters per group
    pub clusters_per_group: u32,
    /// \# Inodes per group
    pub inodes_per_group: u32,
    /// Mount time
    pub mtime: u32,
    /*30*/
    /// Write time
    pub wtime: u32,
    /// Mount count
    pub mnt_count: u16,
    /// Maximal mount count
    pub max_mnt_count: u16,
    /// Magic signature
    pub magic: u16,
    /// File system state
    pub state: u16,
    /// Behaviour when detecting errors
    pub errors: u16,
    /// minor revision level
    pub minor_rev_level: u16,
    /*40*/
    /// time of last check
    pub lastcheck: u32,
    /// max. time between checks
    pub checkinterval: u32,
    /// OS
    pub creator_os: u32,
    /// Revision level
    pub rev_level: u32,
    /*50*/
    /// Default uid for reserved blocks
    pub def_resuid: u16,
    /// Default gid for reserved blocks
    pub def_resgid: u16,
    /* These fields are for EXT4_DYNAMIC_REV superblocks only.*/
    /// First non-reserved inode
    pub first_ino: u32,
    /// size of inode structure
    pub inode_size: u16,
    /// block group # of this superblock
    pub block_group_nr: u16,
    /// compatible feature set
    pub feature_compat: u32,
    /*60*/
    /// incompatible feature set
    pub feature_incompat: u32,
    /// readonly-compatible feature set
    pub feature_ro_compat: u32,
    /*68*/
    /// 128-bit uuid for volume
    pub uuid: [u8; 16],
    /*78*/
    /// volume name
    pub volume_name: [u8; 16],
    /*88*/
    /// directory where last mounted
    pub last_mounted: [u8; 64],
    /*C8*/
    /// For compression
    pub algorithm_usage_bitmap: u32,
    /* Performance hints.  Directory preallocation should only
     * happen if the EXT4_FEATURE_COMPAT_DIR_PREALLOC flag is on.*/
    /// Nr of blocks to try to preallocate
    pub prealloc_blocks: u8,
    /// Nr to preallocate for dirs
    pub prealloc_dir_blocks: u8,
    /// Per group desc for online growth
    pub reserved_gdt_blocks: u16,
    /* Journaling support valid if EXT4_FEATURE_COMPAT_HAS_JOURNAL set.*/
    /*D0*/
    /// uuid of journal superblock
    pub journal_uuid: [u8; 16],
    /*E0*/
    /// inode number of journal file
    pub journal_inum: u32,
    /// device number of journal file
    pub journal_dev: u32,
    /// start of list of inodes to delete
    pub last_orphan: u32,
    /// HTREE hash seed
    pub hash_seed: [u32; 4],
    /// Default hash version to use
    pub def_hash_version: u8,
    pub jnl_backup_type: u8,
    /// size of group descriptor
    pub desc_size: u16,
    /*100*/
    pub default_mount_opts: u32,
    /// First metablock block group
    pub first_meta_bg: u32,
    /// When the filesystem was created
    pub mkfs_time: u32,
    /// Backup of the journal inode
    pub jnl_blocks: [u32; 17],
    /* 64bit support valid if EXT4_FEATURE_INCOMPAT_64BIT */
    /*150*/
    /// Blocks count
    pub blocks_count_hi: u32,
    /// Reserved blocks count
    pub r_blocks_count_hi: u32,
    /// Free blocks count
    pub free_blocks_count_hi: u32,
    /// All inodes have at least # bytes
    pub min_extra_isize: u16,
    /// New inodes should reserve # bytes
    pub want_extra_isize: u16,
    /// Miscellaneous flags
    pub flags: u32,
    /// RAID stride
    pub raid_stride: u16,
    /// \# seconds to wait in MMP checking
    pub mmp_update_interval: u16,
    /// Block for multi-mount protection
    pub mmp_block: u64,
    /// blocks on all data disks (N*stride)
    pub raid_stripe_width: u32,
    /// FLEX_BG group size
    pub log_groups_per_flex: u8,
    /// metadata checksum algorithm used
    pub checksum_type: u8,
    /// versioning level for encryption
    pub encryption_level: u8,
    /// Padding to next 32bits
    pub reserved_pad: u8,
    /// nr of lifetime kilobytes written
    pub kbytes_written: u64,
    /// Inode number of active snapshot
    pub snapshot_inum: u32,
    /// sequential ID of active snapshot
    pub snapshot_id: u32,
    /// reserved blocks for active snapshot's future use
    pub snapshot_r_blocks_count: u64,
    /// inode number of the head of the on-disk snapshot list
    pub snapshot_list: u32,
    /* Error count information */
    pub error_count: u32,
    /// first time an error happened
    pub first_error_time: u32,
    /// inode involved in first error
    pub first_error_ino: u32,
    /// block involved of first error
    pub first_error_block: u64,
    /// function where the error happened
    pub first_error_func: [u8; 32],
    /// line number where error happened
    pub first_error_line: u32,
    /// most recent time of an error
    pub last_error_time: u32,
    /// inode involved in last error
    pub last_error_ino: u32,
    /// line number where error happened
    pub last_error_line: u32,
    /// block involved of last error
    pub last_error_block: u64,
    /// function where the error happened
    pub last_error_func: [u8; 32],
    /// mount options
    pub mount_opts: [u8; 64],
    /// inode for tracking user quota
    pub usr_quota_inum: u32,
    /// inode for tracking group quota
    pub grp_quota_inum: u32,
    /// overhead blocks/clusters in fs
    pub overhead_clusters: u32,
    /// groups with sparse_super2 SBs
    pub backup_bgs: [u32; 2],
    /// Encryption algorithms in use
    pub encrypt_algos: [u8; 4],
    /// Salt used for string2key algorithm
    pub encrypt_pw_salt: [u8; 16],
    /// Location of the lost+found inode
    pub lpf_ino: u32,
    /// inode for tracking project quota
    pub prj_quota_inum: u32,
    /// crc32c(uuid) if csum_seed set
    pub checksum_seed: u32,
    pub wtime_hi: u8,
    pub mtime_hi: u8,
    pub mkfs_time_hi: u8,
    pub lastcheck_hi: u8,
    pub first_error_time_hi: u8,
    pub last_error_time_hi: u8,
    pub first_error_errcode: u8,
    pub last_error_errcode: u8,
    /// Filename charset encoding
    pub encoding: u16,
    /// Filename charset encoding flags
    pub encoding_flags: u16,
    /// Inode for tracking orphan inodes
    pub orphan_file_inum: u32,
    pub def_resuid_hi: u16,
    pub def_resgid_hi: u16,
    pub reserved: [u32; 93],
    /// crc32c(superblock)
    pub checksum: u32,
}

unsafe impl Pod for Ext4SuperBlock {}
