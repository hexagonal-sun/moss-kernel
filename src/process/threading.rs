use core::ffi::c_long;
use core::mem::size_of;

use crate::memory::uaccess::copy_from_user;
use crate::sched::current_task;
use crate::sync::{OnceLock, SpinLock};
use alloc::{collections::BTreeMap, sync::Arc};
use libkernel::sync::waker_set::{WakerSet, wait_until};
use libkernel::{
    error::{KernelError, Result},
    memory::address::{TUA, VA},
};

/// A per-futex wait queue holding wakers for blocked tasks.
struct FutexWaitQueue {
    wakers: WakerSet,
    /// Number of pending wake-ups for waiters on this futex.
    wakeups: usize,
}

impl FutexWaitQueue {
    fn new() -> Self {
        Self {
            wakers: WakerSet::new(),
            wakeups: 0,
        }
    }
}

/// Global futex table mapping a user address to its wait queue.
// TODO: statically allocate an array of SpinLock<Vec<FutexWaitQueue>>.
// Then hash into that table to find it's bucket
// TODO: Should be physical address, not user address
#[allow(clippy::type_complexity)]
static FUTEX_TABLE: OnceLock<SpinLock<BTreeMap<TUA<u32>, Arc<SpinLock<FutexWaitQueue>>>>> =
    OnceLock::new();

pub async fn sys_set_tid_address(_tidptr: VA) -> Result<usize> {
    let tid = current_task().tid;

    // TODO: implement threading and this system call properly. For now, we just
    // return the PID as the thread id.
    Ok(tid.value() as _)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RobustList {
    next: TUA<RobustList>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RobustListHead {
    list: RobustList,
    futex_offset: c_long,
    list_op_pending: RobustList,
}

pub async fn sys_set_robust_list(head: TUA<RobustListHead>, len: usize) -> Result<usize> {
    if core::hint::unlikely(len != size_of::<RobustListHead>()) {
        return Err(KernelError::InvalidValue);
    }

    let task = current_task();
    task.robust_list.lock_save_irq().replace(head);

    Ok(0)
}

const FUTEX_WAIT: i32 = 0;
const FUTEX_WAKE: i32 = 1;
const FUTEX_WAIT_BITSET: i32 = 9;
const FUTEX_WAKE_BITSET: i32 = 10;
const FUTEX_PRIVATE_FLAG: i32 = 128;

pub async fn sys_futex(
    uaddr: TUA<u32>,
    op: i32,
    val: u32,
    _timeout: VA,
    _uaddr2: TUA<u32>,
    _val3: u32,
) -> Result<usize> {
    // Strip PRIVATE flag if present
    let cmd = op & !FUTEX_PRIVATE_FLAG;

    // TODO: support bitset variants properly

    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            // Ensure the wait-queue exists *before* we begin checking the
            // futex word so that a racing FUTEX_WAKE cannot miss us.  This
            // avoids the classic lost-wake-up race where a waker runs between
            // our value check and queue insertion.
            //
            // After publishing the queue we perform a second sanity check on
            // the user word, mirroring Linux’s “double read” strategy.

            // Obtain (or create) the wait-queue for this futex word.
            let table = FUTEX_TABLE.get_or_init(|| SpinLock::new(BTreeMap::new()));
            let waitq_arc = {
                let mut guard = table.lock_save_irq();
                guard
                    .entry(uaddr)
                    .or_insert_with(|| Arc::new(SpinLock::new(FutexWaitQueue::new())))
                    .clone()
            };

            let current: u32 = copy_from_user(uaddr).await?;
            if current != val {
                return Err(KernelError::TryAgain);
            }

            // TODO: When we have try_ variants of locking primitives, use them here
            wait_until(
                waitq_arc.clone(),
                |state| &mut state.wakers,
                |state| {
                    if state.wakeups > 0 {
                        state.wakeups -= 1;
                        Some(())
                    } else {
                        None
                    }
                },
            )
            .await;

            Ok(0)
        }

        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let nr_wake = val as usize;
            let mut woke = 0;

            if let Some(table) = FUTEX_TABLE.get()
                && let Some(waitq_arc) = table.lock_save_irq().get(&uaddr).cloned()
            {
                let mut waitq = waitq_arc.lock_save_irq();
                for _ in 0..nr_wake {
                    // Record a pending wake-up and attempt to wake a single waiter.
                    waitq.wakeups = waitq.wakeups.saturating_add(1);
                    waitq.wakers.wake_one();
                    woke += 1;
                }
            }

            Ok(woke)
        }

        _ => Err(KernelError::NotSupported),
    }
}
