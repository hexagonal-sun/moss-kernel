//! Memory management primitives.
//!
//! This module contains the core building blocks for kernel memory management:
//! typed addresses, memory regions, page frame tracking, page allocators,
//! address-space abstractions, and kernel buffers.
//!
//! The always-available submodules ([`address`], [`page`], [`region`]) require
//! no features. Higher-level subsystems are gated behind their respective
//! feature flags.

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

/// The system page size in bytes (4 KiB).
pub const PAGE_SIZE: usize = 4096;
/// The number of bits to shift to convert between byte offsets and page numbers.
pub const PAGE_SHIFT: usize = PAGE_SIZE.trailing_zeros() as usize;
/// Bitmask for extracting the within-page offset from an address.
pub const PAGE_MASK: usize = PAGE_SIZE - 1;
