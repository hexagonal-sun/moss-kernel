//! The futex2 syscall family: `futex_waitv`, `futex_wake`, `futex_wait` and
//! `futex_requeue`.
//!
//! These share the wait/wake core with the legacy `futex` syscall; only the
//! argument decoding differs. Timeouts are absolute against the given clock,
//! unlike most of the legacy ops.

use alloc::vec::Vec;
use core::time::Duration;

use libkernel::error::{KernelError, Result};
use libkernel::memory::address::TUA;

use super::key::FutexKey;
use super::wait::{ParsedWaiter, futex_wait_multi};
use super::{futex_wait_single, requeue_key, wake_key};
use crate::clock::realtime::date;
use crate::clock::timespec::TimeSpec;
use crate::drivers::timer::uptime;
use crate::memory::uaccess::{UserCopyable, copy_obj_array_from_user};
use crate::sched::syscall_ctx::ProcessCtx;

const FUTEX2_SIZE_U32: u32 = 0x02;
const FUTEX2_SIZE_MASK: u32 = 0x03;
const FUTEX2_PRIVATE: u32 = 0x80;
const FUTEX2_VALID_MASK: u32 = FUTEX2_SIZE_MASK | FUTEX2_PRIVATE;

const FUTEX_WAITV_MAX: usize = 128;
const FUTEX_BITSET_MATCH_ANY: u64 = 0xffff_ffff;

const CLOCK_REALTIME: u32 = 0;
const CLOCK_MONOTONIC: u32 = 1;

/// Userspace `struct futex_waitv`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FutexWaitvUser {
    val: u64,
    uaddr: u64,
    flags: u32,
    reserved: u32,
}

// SAFETY: `FutexWaitvUser` is a plain-old-data `repr(C)` struct; any bit
// pattern is a valid value (validation happens after the copy).
unsafe impl UserCopyable for FutexWaitvUser {}

/// Validates a futex2 flags word, returning whether `FUTEX2_PRIVATE` is set.
///
/// Only 32-bit futexes are supported (as in Linux today), so any size other
/// than `FUTEX2_SIZE_U32` is rejected.
fn check_flags(flags: u32) -> Result<bool> {
    if flags & !FUTEX2_VALID_MASK != 0 {
        return Err(KernelError::InvalidValue);
    }

    if flags & FUTEX2_SIZE_MASK != FUTEX2_SIZE_U32 {
        return Err(KernelError::InvalidValue);
    }

    Ok(flags & FUTEX2_PRIVATE != 0)
}

/// Builds a [`FutexKey`] for a futex2 user address, enforcing the natural
/// alignment futex2 requires.
fn make_key(ctx: &ProcessCtx, uaddr: u64, private: bool) -> Result<(FutexKey, TUA<u32>)> {
    let addr = TUA::<u32>::from_value(uaddr as usize);

    if addr.is_null() {
        return Err(KernelError::Fault);
    }

    if !uaddr.is_multiple_of(core::mem::size_of::<u32>() as u64) {
        return Err(KernelError::InvalidValue);
    }

    let key = if private {
        FutexKey::new_private(ctx, addr)
    } else {
        FutexKey::new_shared(ctx, addr)?
    };

    Ok((key, addr))
}

/// Converts a futex2 absolute timeout into a relative [`Duration`].
///
/// The clockid is only validated when a timeout is supplied, matching Linux.
/// A deadline already in the past yields a zero timeout; the futex value is
/// still checked first, so `EAGAIN` takes precedence over `ETIMEDOUT`.
///
/// `CLOCK_REALTIME` deadlines are converted to a relative sleep up front, so
/// a concurrent `clock_settime` does not retarget an in-progress wait (same
/// simplification as the legacy `FUTEX_WAIT_BITSET` path).
async fn abs_timeout(timeout: TUA<TimeSpec>, clockid: u32) -> Result<Option<Duration>> {
    if timeout.is_null() {
        return Ok(None);
    }

    let deadline = Duration::from(TimeSpec::copy_from_user(timeout).await?);

    let base = match clockid {
        CLOCK_REALTIME => date(),
        CLOCK_MONOTONIC => uptime(),
        _ => return Err(KernelError::InvalidValue),
    };

    Ok(Some(deadline.saturating_sub(base)))
}

/// `futex_wait(uaddr, val, mask, flags, timeout, clockid)`: wait on a single
/// futex word while `*uaddr == val`. Returns 0 once woken by a wake whose
/// mask overlaps `mask`.
pub async fn sys_futex_wait(
    ctx: &ProcessCtx,
    uaddr: u64,
    val: u64,
    mask: u64,
    flags: u32,
    timeout: TUA<TimeSpec>,
    clockid: u32,
) -> Result<usize> {
    let private = check_flags(flags)?;

    // Only 32-bit futexes exist, so values and masks must fit in 32 bits and
    // an empty mask could never be woken.
    if val > u64::from(u32::MAX) || mask > u64::from(u32::MAX) || mask == 0 {
        return Err(KernelError::InvalidValue);
    }

    let timeout = abs_timeout(timeout, clockid).await?;
    let (key, uaddr) = make_key(ctx, uaddr, private)?;

    let waiter = ParsedWaiter {
        key,
        uaddr,
        val: val as u32,
        mask: mask as u32,
    };

    futex_wait_single(waiter, timeout).await
}

/// `futex_wake(uaddr, mask, nr, flags)`: wake up to `nr` waiters whose masks
/// overlap `mask`. Returns the number woken.
pub fn sys_futex_wake(
    ctx: &ProcessCtx,
    uaddr: u64,
    mask: u64,
    nr: i32,
    flags: u32,
) -> Result<usize> {
    let private = check_flags(flags)?;

    if mask > u64::from(u32::MAX) || mask == 0 {
        return Err(KernelError::InvalidValue);
    }

    // Waking zero (or fewer) waiters is a no-op; short-circuit before
    // computing the key, which can fault when translating a shared address.
    if nr <= 0 {
        return Ok(0);
    }

    let (key, _) = make_key(ctx, uaddr, private)?;

    Ok(wake_key(nr as usize, key, mask as u32))
}

/// `futex_waitv(waiters, nr_futexes, flags, timeout, clockid)`: wait on up to
/// [`FUTEX_WAITV_MAX`] futexes at once. Returns the array index of the woken
/// waiter.
pub async fn sys_futex_waitv(
    ctx: &ProcessCtx,
    uwaiters: TUA<FutexWaitvUser>,
    nr_futexes: u32,
    flags: u32,
    timeout: TUA<TimeSpec>,
    clockid: u32,
) -> Result<usize> {
    // No syscall-level flags are defined.
    if flags != 0 {
        return Err(KernelError::InvalidValue);
    }

    if nr_futexes == 0 || nr_futexes as usize > FUTEX_WAITV_MAX {
        return Err(KernelError::InvalidValue);
    }

    let timeout = abs_timeout(timeout, clockid).await?;

    let entries = copy_obj_array_from_user(uwaiters, nr_futexes as usize).await?;

    let mut waiters = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.reserved != 0 || entry.val > u64::from(u32::MAX) {
            return Err(KernelError::InvalidValue);
        }

        let private = check_flags(entry.flags)?;
        let (key, uaddr) = make_key(ctx, entry.uaddr, private)?;

        waiters.push(ParsedWaiter {
            key,
            uaddr,
            val: entry.val as u32,
            mask: FUTEX_BITSET_MATCH_ANY as u32,
        });
    }

    futex_wait_multi(&waiters, timeout).await
}

/// `futex_requeue(waiters, flags, nr_wake, nr_requeue)`: wake up to `nr_wake`
/// waiters on `waiters[0].uaddr`, then move up to `nr_requeue` of the rest
/// onto `waiters[1].uaddr`. No value comparison is performed (unlike legacy
/// `FUTEX_CMP_REQUEUE`). Returns the number woken.
pub async fn sys_futex_requeue(
    ctx: &ProcessCtx,
    uwaiters: TUA<FutexWaitvUser>,
    flags: u32,
    nr_wake: i32,
    nr_requeue: i32,
) -> Result<usize> {
    // No syscall-level flags are defined.
    if flags != 0 {
        return Err(KernelError::InvalidValue);
    }

    if nr_wake < 0 || nr_requeue < 0 {
        return Err(KernelError::InvalidValue);
    }

    let entries = copy_obj_array_from_user(uwaiters, 2).await?;

    let resolve = |entry: &FutexWaitvUser| -> Result<FutexKey> {
        if entry.reserved != 0 {
            return Err(KernelError::InvalidValue);
        }

        // `val` is unused by this op and deliberately not validated.
        let private = check_flags(entry.flags)?;
        let (key, _) = make_key(ctx, entry.uaddr, private)?;
        Ok(key)
    };

    let key1 = resolve(&entries[0])?;
    let key2 = resolve(&entries[1])?;

    // Requeueing a futex onto itself is invalid; this also catches distinct
    // virtual addresses that alias the same shared frame.
    if key1 == key2 {
        return Err(KernelError::InvalidValue);
    }

    Ok(requeue_key(
        key1,
        key2,
        nr_wake as usize,
        nr_requeue as usize,
    ))
}
