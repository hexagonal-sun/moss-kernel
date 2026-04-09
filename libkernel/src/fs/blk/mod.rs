//! Block device layer.

pub mod buffer;
#[cfg(feature = "paging")]
pub mod ramdisk;
