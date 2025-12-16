use crate::pod::Pod;

#[repr(C)]
pub struct Ext4BlockGroupDescriptor {
    /// Blocks bitmap block
    pub block_bitmap_lo: u32,
    /// Inodes bitmap block
    pub inode_bitmap_lo: u32,
    /// Inodes table block
    pub inode_table_lo: u32,
    /// Free blocks count
    pub free_blocks_count_lo: u16,
    /// Free inodes count
    pub free_inodes_count_lo: u16,
    /// Directories count
    pub used_dirs_count_lo: u16,
    /// EXT4_BG_flags (INODE_UNINIT, etc)
    pub flags: u16,
    /// Exclude bitmap for snapshots
    pub exclude_bitmap_lo: u32,
    /// crc32c(s_uuid+grp_num+bbitmap) LE
    pub block_bitmap_csum_lo: u16,
    /// crc32c(s_uuid+grp_num+ibitmap) LE
    pub inode_bitmap_csum_lo: u16,
    /// Unused inodes count
    pub itable_unused_lo: u16,
    /// crc16(sb_uuid+group+desc)
    pub checksum: u16,
    /// Blocks bitmap block MSB
    pub block_bitmap_hi: u32,
    /// Inodes bitmap block MSB
    pub inode_bitmap_hi: u32,
    /// Inodes table block MSB
    pub inode_table_hi: u32,
    /// Free blocks count MSB
    pub free_blocks_count_hi: u16,
    /// Free inodes count MSB
    pub free_inodes_count_hi: u16,
    /// Directories count MSB
    pub used_dirs_count_hi: u16,
    /// Unused inodes count MSB
    pub itable_unused_hi: u16,
    /// Exclude bitmap block MSB
    pub exclude_bitmap_hi: u32,
    /// crc32c(s_uuid+grp_num+bbitmap) BE
    pub block_bitmap_csum_hi: u16,
    /// crc32c(s_uuid+grp_num+ibitmap) BE
    pub inode_bitmap_csum_hi: u16,
    pub reserved: u32,
}

unsafe impl Pod for Ext4BlockGroupDescriptor {}
