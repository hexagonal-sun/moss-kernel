use alloc::sync::Arc;
use libkernel::sync::waker_set::WakerSet;

use crate::sync::SpinLock;

/// A futex wait queue: the set of waiters parked on one [`FutexKey`].
///
/// [`FutexKey`]: super::key::FutexKey
pub type FutexQueue = Arc<SpinLock<WakerSet<Arc<WaiterCell>>>>;

/// Per-waiter shared state, stored both in the waiting future and as the
/// data payload of its [`WakerSet`] entry.
///
/// The cell tracks which queue currently holds the waiter, which is what
/// keeps requeueing sound: `futex_requeue` can move an entry to another
/// queue while the waiting future is asleep, and the future still finds its
/// real location when it cancels or completes.
///
/// # Lock ordering
///
/// Futex table lock → queue lock(s) (ordered by `FutexKey` when taking two)
/// → cell lock. Code that only knows the cell and needs the queue
/// ([`Self::unregister`]) snapshots the location under the cell lock, drops
/// it, locks the queue, then re-locks the cell and verifies the location is
/// unchanged — retrying if a concurrent requeue moved it. Queue tokens are
/// allocated monotonically, so a stale `(queue, token)` snapshot can never
/// alias a new registration.
pub struct WaiterCell {
    /// Wake mask; a wake with mask `m` wakes this waiter iff `mask & m != 0`.
    pub mask: u64,
    state: SpinLock<CellState>,
}

struct CellState {
    /// `Some((queue, token))` while enqueued; `None` once woken or
    /// unregistered.
    location: Option<(FutexQueue, u64)>,
    /// Set when a waker removed this entry, distinguishing a genuine wake
    /// from self-unregistration.
    woken: bool,
}

impl WaiterCell {
    pub fn new(mask: u64) -> Arc<Self> {
        Arc::new(Self {
            mask,
            state: SpinLock::new(CellState {
                location: None,
                woken: false,
            }),
        })
    }

    pub fn is_woken(&self) -> bool {
        self.state.lock_save_irq().woken
    }

    /// Records the queue entry for this waiter. Caller must hold the lock of
    /// `queue` (the registration and the location update must be atomic with
    /// respect to wake/requeue).
    pub fn set_location(&self, queue: FutexQueue, token: u64) {
        self.state.lock_save_irq().location = Some((queue, token));
    }

    /// Marks the waiter woken and detaches it from its queue. Caller must
    /// hold the lock of the queue it was just removed from.
    pub fn mark_woken(&self) {
        let mut state = self.state.lock_save_irq();
        state.woken = true;
        state.location = None;
    }

    /// Updates the location after the entry moved to `queue`. Caller must
    /// hold the locks of both the source and destination queues.
    pub fn requeue_to(&self, queue: FutexQueue, token: u64) {
        self.state.lock_save_irq().location = Some((queue, token));
    }

    /// Removes this waiter from whichever queue currently holds it.
    ///
    /// Returns `true` if a wake landed first (that wake is then consumed by
    /// the caller).
    pub fn unregister(&self) -> bool {
        loop {
            // Snapshot the location; we cannot lock the queue while holding
            // the cell lock without inverting the queue → cell order.
            let (queue, token) = {
                let state = self.state.lock_save_irq();
                match &state.location {
                    Some((queue, token)) => (queue.clone(), *token),
                    None => return state.woken,
                }
            };

            let mut queue_guard = queue.lock_save_irq();
            let mut state = self.state.lock_save_irq();

            match &state.location {
                None => return state.woken,
                Some((q, t)) if Arc::ptr_eq(q, &queue) && *t == token => {
                    queue_guard.remove(token);
                    state.location = None;
                    return false;
                }
                // Requeued between the snapshot and taking the queue lock.
                _ => continue,
            }
        }
    }
}
