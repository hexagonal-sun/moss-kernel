pub mod address;
#[cfg(feature = "paging")]
pub mod address_space;
#[cfg(feature = "alloc")]
pub mod allocators;
#[cfg(feature = "alloc")]
pub mod claimed_page;
#[cfg(feature = "kbuf")]
pub mod kbuf;
pub mod page;
#[cfg(feature = "paging")]
pub mod permissions;
#[cfg(feature = "paging")]
pub mod pg_offset;
#[cfg(feature = "proc_vm")]
pub mod proc_vm;
pub mod region;

pub const PAGE_SIZE: usize = 4096;
pub const PAGE_SHIFT: usize = PAGE_SIZE.trailing_zeros() as usize;
pub const PAGE_MASK: usize = PAGE_SIZE - 1;
