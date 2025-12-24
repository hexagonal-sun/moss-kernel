use crate::clock::realtime::date;
use crate::clock::timespec::TimeSpec;
use crate::drivers::timer::sleep;
use crate::sync::{OnceLock, SpinLock};
use alloc::boxed::Box;
use alloc::{collections::btree_map::BTreeMap, sync::Arc};
use core::time::Duration;
use futures::FutureExt;
use key::FutexKey;
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
    sync::waker_set::WakerSet,
};
use wait::FutexWait;

pub mod key;
mod wait;

const FUTEX_WAIT: i32 = 0;
const FUTEX_WAKE: i32 = 1;
const FUTEX_WAIT_BITSET: i32 = 9;
const FUTEX_WAKE_BITSET: i32 = 10;
const FUTEX_PRIVATE_FLAG: i32 = 128;

type FutexTable = BTreeMap<FutexKey, Arc<SpinLock<WakerSet>>>;

/// Global futex table mapping a futex key to its wait queue.
#[allow(clippy::type_complexity)]
static FUTEX_TABLE: OnceLock<SpinLock<FutexTable>> = OnceLock::new();

fn futex_table() -> &'static SpinLock<FutexTable> {
    FUTEX_TABLE.get_or_init(|| SpinLock::new(BTreeMap::new()))
}

fn get_or_create_queue(key: FutexKey) -> Arc<SpinLock<WakerSet>> {
    let table = futex_table();

    table
        .lock_save_irq()
        .entry(key)
        .or_insert_with(|| Arc::new(SpinLock::new(WakerSet::new())))
        .clone()
}

pub fn wake_key(nr_wake: usize, key: FutexKey) -> usize {
    let mut woke = 0;

    let table = futex_table();

    if let Some(waitq_arc) = table.lock_save_irq().get(&key).cloned() {
        let mut waitq = waitq_arc.lock_save_irq();
        for _ in 0..nr_wake {
            if waitq.wake_one() {
                woke += 1;
            } else {
                break;
            }
        }
    }

    woke
}

pub async fn sys_futex(
    uaddr: TUA<u32>,
    op: i32,
    val: u32,
    timeout: TUA<TimeSpec>,
    _uaddr2: TUA<u32>,
    _val3: u32,
) -> Result<usize> {
    // Strip PRIVATE flag if present
    let cmd = op & !FUTEX_PRIVATE_FLAG;

    let key = if op & FUTEX_PRIVATE_FLAG != 0 {
        FutexKey::new_private(uaddr)
    } else {
        FutexKey::new_shared(uaddr)?
    };

    // TODO: support bitset variants properly
    let timeout = if timeout.is_null() {
        None
    } else {
        let timeout = TimeSpec::copy_from_user(timeout).await?;
        if matches!(cmd, FUTEX_WAIT_BITSET) {
            Some(Duration::from(timeout) - date())
        } else {
            Some(Duration::from(timeout))
        }
    };
    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            // Obtain (or create) the wait-queue for this futex word.
            let slot = get_or_create_queue(key);

            // Return 0 on success.
            if let Some(dur) = timeout {
                let mut wait = FutexWait::new(uaddr, val, slot).fuse();
                let mut sleep = Box::pin(sleep(dur).fuse());
                futures::select_biased! {
                    res = wait => {
                        res.map(|_| 0)
                    },
                    _ = sleep => {
                        Err(KernelError::TimedOut)
                    }
                }
            } else {
                FutexWait::new(uaddr, val, slot).await.map(|_| 0)
            }
        }

        FUTEX_WAKE | FUTEX_WAKE_BITSET => Ok(wake_key(val as _, key)),

        _ => Err(KernelError::NotSupported),
    }
}
