//! Plain Old Data trait and blanket implementations.
//!
//! Types that implement [`Pod`] can be safely created by copying their raw byte
//! representation. This is useful for reading on-disk structures from block
//! devices.

/// An unsafe trait indicating that a type is "Plain Old Data".
///
/// A type is `Pod` if it is a simple collection of bytes with no invalid bit
/// patterns. This means it can be safely created by simply copying its byte
/// representation from memory or a device.
///
/// # Safety
///
/// The implementor of this trait MUST guarantee that:
/// 1. The type has a fixed, known layout. Using `#[repr(C)]` or
///    `#[repr(transparent)]` is a must! The Rust ABI is unstable.
/// 2. The type contains no padding bytes, or if it does, that reading those
///    padding bytes as uninitialized memory is not undefined behavior.
/// 3. All possible bit patterns for the type's size are valid instances of the type.
///    For example, a `bool` is NOT `Pod` because its valid representations are only
///    0x00 and 0x01, not any other byte value. A `u32` is `Pod` because all
///    2^32 bit patterns are valid `u32` values.
pub unsafe trait Pod: Sized {}

// Blanket implementations for primitive types that are definitely Pod.
unsafe impl Pod for u8 {}
unsafe impl Pod for u16 {}
unsafe impl Pod for u32 {}
unsafe impl Pod for u64 {}
unsafe impl Pod for u128 {}
unsafe impl Pod for i8 {}
unsafe impl Pod for i16 {}
unsafe impl Pod for i32 {}
unsafe impl Pod for i64 {}
unsafe impl Pod for i128 {}
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}
