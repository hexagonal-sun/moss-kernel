use crate::pod::Pod;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Ext4InodeOsd1Linux {
    pub l_i_version: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Ext4InodeOsd1Hurd {
    pub h_i_translator: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Ext4InodeOsd1Masix {
    pub m_i_reserved1: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union Ext4InodeOsd1 {
    pub linux1: Ext4InodeOsd1Linux,
    pub hurd1: Ext4InodeOsd1Hurd,
    pub masix1: Ext4InodeOsd1Masix,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Ext4InodeOsd2Linux {
    pub l_i_blocks_high: u16,
    pub l_i_file_acl_high: u16,
    pub l_i_uid_high: u16,
    pub l_i_gid_high: u16,
    pub l_i_checksum_lo: u16,
    pub l_i_reserved: u16,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Ext4InodeOsd2Hurd {
    pub h_i_reserved1: u16,
    pub h_i_mode_high: u16,
    pub h_i_uid_high: u16,
    pub h_i_gid_high: u16,
    pub h_i_author: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Ext4InodeOsd2Masix {
    pub h_i_reserved1: u16,
    pub m_i_file_acl_high: u16,
    pub m_i_reserved2: [u32; 2],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union Ext4InodeOsd2 {
    pub linux2: Ext4InodeOsd2Linux,
    pub hurd2: Ext4InodeOsd2Hurd,
    pub masix2: Ext4InodeOsd2Masix,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Ext4Inode {
    /// File mode
    pub mode: u16,
    /// Low 16 bits of Owner Uid
    pub uid: u16,
    /// Size in bytes
    pub size_lo: u32,
    /// Access time
    pub atime: u32,
    /// Inode Change time
    pub ctime: u32,
    /// Modification time
    pub mtime: u32,
    /// Deletion Time
    pub dtime: u32,
    /// Low 16 bits of Group Id
    pub gid: u16,
    /// Links count
    pub links_count: u16,
    /// Blocks count
    pub blocks_lo: u32,
    /// File flags
    pub flags: u32,
    /// OS dependent 1
    pub osd1: Ext4InodeOsd1,
    /// Pointers to blocks
    pub block: [u32; 15],
    /// File version (for NFS)
    pub generation: u32,
    /// File ACL
    pub file_acl_lo: u32,
    pub size_high: u32,
    /// Obsoleted fragment address
    pub obso_faddr: u32,
    /// OS dependent 2
    pub osd2: Ext4InodeOsd2,
    pub extra_isize: u16,
    /// crc32c(uuid+inum+inode) BE
    pub checksum_hi: u16,
    /// extra Change time      (nsec << 2 | epoch)
    pub ctime_extra: u32,
    /// extra Modification time(nsec << 2 | epoch)
    pub mtime_extra: u32,
    /// extra Access time      (nsec << 2 | epoch)
    pub atime_extra: u32,
    /// File Creation time
    pub crtime: u32,
    /// extra FileCreationtime (nsec << 2 | epoch)
    pub crtime_extra: u32,
    /// high 32 bits for 64-bit version
    pub version_hi: u32,
    /// Project ID
    pub projid: u32,
}

unsafe impl Pod for Ext4InodeOsd1Linux {}
unsafe impl Pod for Ext4InodeOsd1Hurd {}
unsafe impl Pod for Ext4InodeOsd1Masix {}
unsafe impl Pod for Ext4InodeOsd1 {}
unsafe impl Pod for Ext4InodeOsd2Linux {}
unsafe impl Pod for Ext4InodeOsd2Hurd {}
unsafe impl Pod for Ext4InodeOsd2Masix {}
unsafe impl Pod for Ext4InodeOsd2 {}
unsafe impl Pod for Ext4Inode {}
