use core::mem::MaybeUninit;

use crate::arch::{Arch, ArchImpl};
use alloc::vec::Vec;
use libkernel::error::Result;
use libkernel::memory::address::{TUA, UA};

pub mod cstr;

/// A marker trait for types that are safe to copy to or from userspace.
///
/// # Safety
///
/// Implementing this trait is `unsafe` because the developer must guarantee
/// that the type meets the following criteria:
///
/// 1. No sensitive information: The type must not contain any data that could
///    reveal kernel memory layouts, kernel pointers, or other sensitive
///    information.
///
/// 2. Stable representation: The type should have a stable memory layout that
///    is understood by both the kernel and userspace. Use `#[repr(C)]` or
///    `#[repr(transparent)]`.
///
/// This trait is a contract that ensures a type is fundamentally "plain old
/// data" (POD) and is safe to treat as a simple bag of bytes for user/kernel
/// communication.
pub unsafe trait UserCopyable: Copy {}

pub async fn copy_to_user<T: UserCopyable>(dst: TUA<T>, obj: T) -> Result<()> {
    unsafe {
        ArchImpl::copy_to_user(
            (&obj) as *const _ as *const _,
            dst.to_untyped(),
            core::mem::size_of::<T>(),
        )
        .await
    }
}

pub async fn copy_from_user<T: UserCopyable>(src: TUA<T>) -> Result<T> {
    let mut uninit: MaybeUninit<T> = MaybeUninit::uninit();

    unsafe {
        ArchImpl::copy_from_user(
            src.to_untyped(),
            uninit.as_mut_ptr() as *mut _ as *mut _,
            core::mem::size_of::<T>(),
        )
        .await?
    };

    // SAFETY: If the future completed successfully, then the copy from
    // userspace completed.
    Ok(unsafe { uninit.assume_init() })
}

pub fn try_copy_from_user<T: UserCopyable>(src: TUA<T>) -> Result<T> {
    let mut uninit: MaybeUninit<T> = MaybeUninit::uninit();

    unsafe {
        ArchImpl::try_copy_from_user(
            src.to_untyped(),
            uninit.as_mut_ptr() as *mut _ as *mut _,
            core::mem::size_of::<T>(),
        )
    }?;

    // SAFETY: If the `try_copy_from_user` completed successfully, then the copy
    // from userspace completed.
    Ok(unsafe { uninit.assume_init() })
}

pub async fn copy_obj_array_from_user<T: UserCopyable>(
    mut src: TUA<T>,
    len: usize,
) -> Result<Vec<T>> {
    let mut ret = Vec::with_capacity(len);

    for _ in 0..len {
        ret.push(copy_from_user(src).await?);
        src = src.add_objs(1);
    }

    Ok(ret)
}

pub async fn copy_objs_to_user<T: UserCopyable>(src: &[T], mut dst: TUA<T>) -> Result<()> {
    for obj in src {
        copy_to_user(dst, *obj).await?;
        dst = dst.add_objs(1);
    }

    Ok(())
}

pub async fn copy_from_user_slice(src: UA, dst: &mut [u8]) -> Result<()> {
    unsafe { ArchImpl::copy_from_user(src, dst.as_mut_ptr() as *mut _ as *mut _, dst.len()).await }
}

pub async fn copy_to_user_slice(src: &[u8], dst: UA) -> Result<()> {
    unsafe { ArchImpl::copy_to_user(src.as_ptr().cast(), dst, src.len()).await }
}

macro_rules! impl_user_copyable_for_primitives {
    ($($t:ty),*) => {
        $(
            // SAFETY: Primitive types are POD, have no padding bytes,
            // and contain no sensitive information. They are safe to
            // copy between userspace and kernel.
            unsafe impl UserCopyable for $t {}
        )*
    };
}

impl_user_copyable_for_primitives! {
    u8, i8, u16, i16, u32, i32, u64, i64, u128, i128, usize, isize, f32, f64, bool, char
}

// SAFETY: An array [T; N] is a contiguous sequence of N elements of type T. If
// the type T is itself safe to copy, then a contiguous block of them is also
// safe to copy.
unsafe impl<T: UserCopyable, const N: usize> UserCopyable for [T; N] {}

// Copying a pointer to another pointer which points to a `UserCopyable` type is
// safe to copy.
unsafe impl<T: UserCopyable> UserCopyable for TUA<T> {}
