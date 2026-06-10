use crate::clock::realtime::date;
use crate::clock::timespec::TimeSpec;
use crate::process::thread_group::signal::{InterruptResult, Interruptable};
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

pub fn wake_key(nr_wake: usize, key: FutexKey, mask: u64) -> usize {
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
    if key1 == key2 {
        // Requeueing onto the same queue is a no-op; just wake.
        return wake_key(nr_wake, key1, u64::MAX);
    }

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

async fn do_futex_wait(
    key: FutexKey,
    uaddr: TUA<u32>,
    val: u32,
    bitmask: u32,
    timeout: Option<Duration>,
) -> Result<usize> {
    let waiter = ParsedWaiter {
        key,
        uaddr,
        val,
        mask: bitmask as u64,
    };

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
                let timeout = TimeSpec::copy_from_user(timeout).await?;
                if matches!(cmd, FUTEX_WAIT_BITSET) {
                    // The deadline is absolute and may already have passed;
                    // a zero timeout still performs the value check below.
                    Some(Duration::from(timeout).saturating_sub(date()))
                } else {
                    Some(Duration::from(timeout))
                }
            };

            match do_futex_wait(
                key,
                uaddr,
                val,
                if cmd == FUTEX_WAIT { u32::MAX } else { val3 },
                timeout,
            )
            .interruptable()
            .await
            {
                InterruptResult::Interrupted => Err(KernelError::Interrupted),
                InterruptResult::Uninterrupted(v) => v,
            }
        }

        FUTEX_WAKE | FUTEX_WAKE_BITSET => Ok(wake_key(
            val as _,
            key,
            if cmd == FUTEX_WAKE {
                u32::MAX as u64
            } else {
                val3 as u64
            },
        )),

        _ => Err(KernelError::NotSupported),
    }
}
