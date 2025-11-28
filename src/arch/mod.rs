//! The architectural abstraction layer.
//!
//! This module defines the `Arch` trait, which encapsulates all
//! architecture-specific functionality required by the kernel. To port the
//! kernel to a new architecture, a new submodule must be created here which
//! implements this trait.
//!
//! The rest of the kernel should use the `ArchImpl` type alias to access
//! architecture-specific functions and types.

use crate::process::{
    Task,
    thread_group::signal::{SigId, ksigaction::UserspaceSigAction},
};
use alloc::sync::Arc;
use libkernel::{
    CpuOps, VirtualMemory,
    error::Result,
    memory::address::{UA, VA},
};

pub trait Arch: CpuOps + VirtualMemory {
    /// The type representing the state saved to the stack on an exception or
    /// context switch. The kernel's scheduler and exception handlers will work
    /// with this type.
    type UserContext: Sized + Send + Sync + Clone;

    fn name() -> &'static str;

    /// Prepares the initial context for a new user-space thread. This sets up
    /// the stack frame so that when we context-switch to it, it will begin
    /// execution at the specified `entry_point`.
    fn new_user_context(entry_point: VA, stack_top: VA) -> Self::UserContext;

    /// Switch the current CPU's context to `new`, setting `new` to be the next
    /// task to be executed.
    fn context_switch(new: Arc<Task>);

    /// Construct a new idle task.
    fn create_idle_task() -> Task;

    /// Powers off the machine. Implementations must never return.
    fn power_off() -> !;

    /// Call a user-specified signal handler in the current process.
    fn do_signal(
        sig: SigId,
        action: UserspaceSigAction,
    ) -> impl Future<Output = Result<<Self as Arch>::UserContext>>;

    /// Return from a userspace signal handler.
    fn do_signal_return() -> impl Future<Output = Result<<Self as Arch>::UserContext>>;

    /// Copies a block of memory from userspace to the kernel.
    ///
    /// This is the raw, unsafe primitive for transferring data from a
    /// user-provided source address `src` into a kernel buffer `dst`. It is the
    /// responsibility of this function to safely handle page faults. If `src`
    /// points to invalid, unmapped, or protected memory, this function must not
    /// panic. Instead, it should catch the fault and return an error.
    ///
    /// # Errors
    ///
    /// Returns `KernelError::Fault` if a page fault occurs while reading from
    /// the userspace address `src`.
    ///
    /// # Safety
    ///
    /// This function is profoundly `unsafe` and is a primary security boundary.
    /// The caller MUST guarantee the following invariants about the `dst`
    /// kernel pointer:
    ///
    /// 1.  `dst` must be a valid pointer.
    /// 2.  The memory region `[dst, dst + len)` must be allocated, writable, and
    ///     contained within kernel memory.
    /// 3.  `dst` must be properly aligned for any subsequent access.
    ///
    /// Failure to uphold these invariants will result in undefined behavior,
    /// likely leading to a kernel panic or memory corruption. Prefer using safe
    /// wrappers like `copy_from_user<T: UserCopyable>` or
    /// `copy_from_user_slice` whenever possible.
    unsafe fn copy_from_user(src: UA, dst: *mut (), len: usize)
    -> impl Future<Output = Result<()>>;

    /// Copies a block of memory from the kernel to userspace.
    ///
    /// This is the raw, unsafe primitive for transferring data from a kernel
    /// buffer `src` to a user-provided destination address `dst`. It is the
    /// responsibility of this function to safely handle page faults. If `dst`
    /// points to invalid, unmapped, or protected memory, this function must not
    /// panic. Instead, it should catch the fault and return an error.
    ///
    /// # Errors
    ///
    /// Returns `KernelError::Fault` if a page fault occurs while writing to the
    /// userspace address `dst`.
    ///
    /// # Security
    ///
    /// The caller is responsible for ensuring that the memory being copied does
    /// not contain sensitive kernel data, such as pointers, internal state, or
    /// uninitialized padding bytes. Leaking such information to userspace is a
    /// serious security vulnerability. Use the `UserCopyable` trait to enforce
    /// this contract at a higher level.
    ///
    /// # Safety
    ///
    /// This function is profoundly `unsafe`. The caller MUST guarantee the
    /// following invariants about the `src` kernel pointer:
    ///
    /// 1. `src` must be a valid pointer.
    /// 2. The memory region `[src, src + len)` must be allocated, readable, and
    ///    initialized. Reading from uninitialized memory is undefined behavior.
    ///
    /// Failure to uphold these invariants can lead to a kernel panic or leak of
    /// sensitive data. Prefer using safe wrappers like `copy_to_user<T:
    /// UserCopyable>` or `copy_to_user_slice` whenever possible.
    unsafe fn copy_to_user(src: *const (), dst: UA, len: usize)
    -> impl Future<Output = Result<()>>;

    /// Copies a null-terminated string from userspace into a kernel buffer.
    ///
    /// Copies at most `len` bytes from the userspace address `src` into the
    /// kernel buffer `dst`. Copying stops when a null terminator is found in
    /// the source string.
    ///
    /// The destination buffer `dst` is guaranteed to be null-terminated on
    /// success.
    ///
    /// # Returns
    ///
    /// On success, returns `Ok(n)`, where `n` is the length of the copied
    /// string (excluding the null terminator).
    ///
    /// # Errors
    ///
    /// - `Error::NameTooLong` (or a similar error): If `len` bytes are read
    ///   from `src` without finding a null terminator.
    /// - `Error::BadAddress`: If a page fault occurs while reading from `src`.
    ///
    /// # Safety
    ///
    /// This function is `unsafe` because it operates on a raw pointer `dst`.
    /// The caller must guarantee that `dst` is a valid pointer and points to a
    /// memory buffer that is at least `len` bytes long.
    unsafe fn copy_strn_from_user(
        src: UA,
        dst: *mut u8,
        len: usize,
    ) -> impl Future<Output = Result<usize>>;
}

#[cfg(target_arch = "aarch64")]
mod arm64;

#[cfg(target_arch = "aarch64")]
pub use self::arm64::Aarch64 as ArchImpl;
