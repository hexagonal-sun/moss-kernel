use crate::clock::Deadline;
use crate::clock::timespec::TimeSpec;
use crate::drivers::timer::uptime;
use crate::sched::syscall_ctx::ProcessCtx;
use crate::sync::{OnceLock, SpinLock};
use alloc::vec::Vec;
use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use core::time::Duration;
use key::FutexKey;
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
    sync::waker_set::WakerSet,
};
use wait::{ParsedWaiter, futex_wait_multi};
use waiter::{FutexQueue, WaiterCell};

pub mod futex2;
pub mod key;
mod wait;
mod waiter;

const FUTEX_WAIT: i32 = 0;
const FUTEX_WAKE: i32 = 1;
const FUTEX_WAIT_BITSET: i32 = 9;
const FUTEX_WAKE_BITSET: i32 = 10;
const FUTEX_PRIVATE_FLAG: i32 = 128;

type FutexTable = BTreeMap<FutexKey, FutexQueue>;

/// Global futex table mapping a futex key to its wait queue.
#[allow(clippy::type_complexity)]
static FUTEX_TABLE: OnceLock<SpinLock<FutexTable>> = OnceLock::new();

fn futex_table() -> &'static SpinLock<FutexTable> {
    FUTEX_TABLE.get_or_init(|| SpinLock::new(BTreeMap::new()))
}

fn get_or_create_queue(key: FutexKey) -> FutexQueue {
    let table = futex_table();

    table
        .lock_save_irq()
        .entry(key)
        .or_insert_with(|| Arc::new(SpinLock::new(WakerSet::new())))
        .clone()
}

pub fn wake_key(nr_wake: usize, key: FutexKey, mask: u32) -> usize {
    let mut wakers = Vec::new();

    let table = futex_table();

    if let Some(waitq_arc) = table.lock_save_irq().get(&key).cloned() {
        let mut waitq = waitq_arc.lock_save_irq();

        while wakers.len() < nr_wake {
            match waitq.take_if(|cell: &Arc<WaiterCell>| cell.mask & mask != 0) {
                Some((waker, cell)) => {
                    cell.mark_woken();
                    wakers.push(waker);
                }
                None => break,
            }
        }
    }

    // Wake outside the queue lock; a woken task may run immediately and
    // re-take futex locks.
    let woke = wakers.len();
    for waker in wakers {
        waker.wake();
    }

    woke
}

/// Wakes up to `nr_wake` waiters on `key1`, then moves up to `nr_requeue` of
/// the remaining waiters onto `key2`'s queue without waking them.
///
/// Wake masks are ignored, matching Linux requeue semantics. Returns the
/// number of waiters woken.
pub fn requeue_key(key1: FutexKey, key2: FutexKey, nr_wake: usize, nr_requeue: usize) -> usize {
    // Callers must reject `key1 == key2` (requeue-to-self) before this point;
    // the two-queue locking below assumes distinct queues.
    debug_assert_ne!(key1, key2);

    let q1_arc = get_or_create_queue(key1);
    let q2_arc = get_or_create_queue(key2);

    let mut wakers = Vec::new();

    {
        // Lock both queues in key order so concurrent requeues can't
        // deadlock.
        let (mut q1, mut q2) = if key1 < key2 {
            let q1 = q1_arc.lock_save_irq();
            let q2 = q2_arc.lock_save_irq();
            (q1, q2)
        } else {
            let q2 = q2_arc.lock_save_irq();
            let q1 = q1_arc.lock_save_irq();
            (q1, q2)
        };

        while wakers.len() < nr_wake {
            match q1.take_first() {
                Some((waker, cell)) => {
                    cell.mark_woken();
                    wakers.push(waker);
                }
                None => break,
            }
        }

        for _ in 0..nr_requeue {
            match q1.take_first() {
                Some((waker, cell)) => {
                    let token = q2.insert(waker, cell.clone());
                    cell.requeue_to(q2_arc.clone(), token);
                }
                None => break,
            }
        }
    }

    let woke = wakers.len();
    for waker in wakers {
        waker.wake();
    }

    woke
}

/// Waits on a single futex word, the common case shared by the legacy
/// `FUTEX_WAIT` ops and futex2 `sys_futex_wait`. Interruption (and recovery of
/// a wake that raced a signal) is handled inside [`futex_wait_multi`].
pub(super) async fn futex_wait_single(
    waiter: ParsedWaiter,
    timeout: Option<Deadline>,
) -> Result<usize> {
    // Return 0 on success.
    futex_wait_multi(core::slice::from_ref(&waiter), timeout)
        .await
        .map(|_| 0)
}

pub async fn sys_futex(
    ctx: &ProcessCtx,
    uaddr: TUA<u32>,
    op: i32,
    val: u32,
    timeout: TUA<TimeSpec>,
    _uaddr2: TUA<u32>,
    val3: u32,
) -> Result<usize> {
    // Strip PRIVATE flag if present
    let cmd = op & !FUTEX_PRIVATE_FLAG;

    let key = if op & FUTEX_PRIVATE_FLAG != 0 {
        FutexKey::new_private(ctx, uaddr)
    } else {
        FutexKey::new_shared(ctx, uaddr)?
    };

    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            let timeout = if timeout.is_null() {
                None
            } else {
                let ts = Duration::from(TimeSpec::copy_from_user(timeout).await?);
                if matches!(cmd, FUTEX_WAIT_BITSET) {
                    // FUTEX_WAIT_BITSET takes an absolute realtime deadline.
                    Some(Deadline::Realtime(ts))
                } else {
                    // FUTEX_WAIT takes a relative timeout on the monotonic
                    // clock; convert to an absolute monotonic deadline.
                    Some(Deadline::Monotonic(uptime() + ts))
                }
            };

            let bitmask = if cmd == FUTEX_WAIT { u32::MAX } else { val3 };

            // A zero bitset can never be matched by any wake, so the waiter
            // would be unwakeable; reject it as Linux does.
            if matches!(cmd, FUTEX_WAIT_BITSET) && bitmask == 0 {
                return Err(KernelError::InvalidValue);
            }

            let waiter = ParsedWaiter {
                key,
                uaddr,
                val,
                mask: bitmask,
            };

            futex_wait_single(waiter, timeout).await
        }

        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let mask = if cmd == FUTEX_WAKE { u32::MAX } else { val3 };

            // A zero bitset matches no waiter; reject it as Linux does.
            if matches!(cmd, FUTEX_WAKE_BITSET) && mask == 0 {
                return Err(KernelError::InvalidValue);
            }

            Ok(wake_key(val as _, key, mask))
        }

        _ => Err(KernelError::NotSupported),
    }
}
