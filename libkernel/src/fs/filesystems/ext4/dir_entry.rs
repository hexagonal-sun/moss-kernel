use crate::fs::filesystems::ext4::EXT4_NAME_LEN;
use crate::pod::Pod;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Ext4DirEntry {
    /// Inode number
    pub inode: u32,
    /// Directory entry length
    pub rec_len: u16,
    /// Name length
    pub name_len: u16,
    /// File name
    pub name: [u8; EXT4_NAME_LEN],
}

unsafe impl Pod for Ext4DirEntry {}
