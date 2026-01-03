use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::{
    pin::Pin,
    task::{Context, Poll, Waker},
};

use crate::CpuOps;

use super::spinlock::SpinLockIrq;

pub struct WakerSet<T = ()> {
    waiters: BTreeMap<u64, (Waker, T)>,
    next_id: u64,
}

impl Default for WakerSet {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> WakerSet<T> {
    pub fn new() -> Self {
        Self {
            waiters: BTreeMap::new(),
            next_id: 0,
        }
    }

    fn allocate_id(&mut self) -> u64 {
        let id = self.next_id;

        // Use wrapping_add to prevent panic on overflow, though it's
        // astronomically unlikely.
        self.next_id = self.next_id.wrapping_add(1);

        id
    }

    pub fn contains_token(&self, token: u64) -> bool {
        self.waiters.contains_key(&token)
    }

    /// Removes a waker using its token.
    pub fn remove(&mut self, token: u64) {
        self.waiters.remove(&token);
    }

    /// Wakes one waiting task, if any.
    ///
    /// Returns `true` if a waker was awoken, `false` otherwise.
    pub fn wake_one(&mut self) -> bool {
        if let Some((_, waker)) = self.waiters.pop_first() {
            waker.0.wake();
            true
        } else {
            false
        }
    }

    /// Wakes all waiting tasks.
    pub fn wake_all(&mut self) {
        for (_, waker) in core::mem::take(&mut self.waiters) {
            waker.0.wake();
        }
    }

    /// Apply `predicate` to wakers in the set. For the first element where
    /// `predicate` returns `true`, `wake()` is called and this function returns
    /// `true`. If `predicate` doesn't match any wakers, `false` is returned.
    pub fn wake_if(&mut self, predicate: impl Fn(&T) -> bool) -> bool {
        if let Some(key) = self
            .waiters
            .iter()
            .find(|(_, (_, data))| predicate(data))
            .map(|(key, _)| *key)
        {
            self.waiters.remove(&key).unwrap().0.wake();

            true
        } else {
            false
        }
    }

    pub fn register_with_data(&mut self, waker: &Waker, data: T) -> u64 {
        let id = self.allocate_id();

        self.waiters.insert(id, (waker.clone(), data));

        id
    }
}

impl WakerSet<()> {
    /// Registers a waker, returning a drop-aware token. When the token is
    /// dropped, the waker is removed from the queue.
    pub fn register(&mut self, waker: &Waker) -> u64 {
        self.register_with_data(waker, ())
    }
}

/// A future that waits until a condition on a shared state is met.
///
/// This future is designed to work with a state `T` protected by a
/// `SpinLockIrq`, where `T` contains one or more `WakerSet`s.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct WaitUntil<C, T, F, G, R>
where
    C: CpuOps,
    F: FnMut(&mut T) -> Option<R>,
    G: FnMut(&mut T) -> &mut WakerSet,
{
    lock: Arc<SpinLockIrq<T, C>>,
    get_waker_set: G,
    predicate: F,
    token: Option<u64>,
}

/// Creates a future that waits on a specific `WakerSet` within a shared,
/// locked state `T` until a condition is met.
pub fn wait_until<C, T, F, G, R>(
    lock: Arc<SpinLockIrq<T, C>>,
    get_waker_set: G,
    predicate: F,
) -> WaitUntil<C, T, F, G, R>
where
    C: CpuOps,
    F: FnMut(&mut T) -> Option<R>,
    G: FnMut(&mut T) -> &mut WakerSet,
{
    WaitUntil {
        lock,
        get_waker_set,
        predicate,
        token: None,
    }
}

impl<C, T, F, G, R> Future for WaitUntil<C, T, F, G, R>
where
    C: CpuOps,
    F: FnMut(&mut T) -> Option<R>,
    G: FnMut(&mut T) -> &mut WakerSet,
{
    type Output = R;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Unsafe is required to move fields out of a pinned struct.
        // This is safe because we are not moving the fields that are
        // required to be pinned (none of them are in this case).
        let this = unsafe { self.get_unchecked_mut() };

        let mut inner = this.lock.lock_save_irq();

        // Check the condition first.
        if let Some(result) = (this.predicate)(&mut inner) {
            return Poll::Ready(result);
        }

        // If the condition is not met, register our waker if we haven't already.
        if this.token.is_none() {
            let waker_set = (this.get_waker_set)(&mut inner);
            let id = waker_set.register(cx.waker());
            this.token = Some(id);
        }

        Poll::Pending
    }
}

impl<C, T, F, G, R> Drop for WaitUntil<C, T, F, G, R>
where
    C: CpuOps,
    F: FnMut(&mut T) -> Option<R>,
    G: FnMut(&mut T) -> &mut WakerSet,
{
    fn drop(&mut self) {
        // If we have a token, it means we're registered in the waker set.
        // We must acquire the lock and remove ourselves.
        if let Some(token) = self.token {
            let mut inner = self.lock.lock_save_irq();
            let waker_set = (self.get_waker_set)(&mut inner);
            waker_set.remove(token);
        }
    }
}

#[cfg(test)]
mod wait_until_tests {
    use super::*;
    use crate::test::MockCpuOps; // Adjust paths
    use std::sync::Arc;
    use std::time::Duration;

    struct SharedState {
        condition_met: bool,
        waker_set: WakerSet,
    }

    #[tokio::test]
    async fn wait_until_completes_when_condition_is_met() {
        let initial_state = SharedState {
            condition_met: false,
            waker_set: WakerSet::new(),
        };

        let lock = Arc::new(SpinLockIrq::<_, MockCpuOps>::new(initial_state));
        let lock_clone = lock.clone();

        let wait_future = wait_until(
            lock.clone(),
            |state| &mut state.waker_set,
            |state| {
                if state.condition_met { Some(()) } else { None }
            },
        );

        let handle = tokio::spawn(wait_future);

        // Give the future a chance to run and register its waker.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // The future should not have completed yet.
        assert!(!handle.is_finished());

        // Now, meet the condition and wake the task.
        {
            let mut state = lock_clone.lock_save_irq();
            state.condition_met = true;
            state.waker_set.wake_one();
        }

        // The future should now complete.
        let result = tokio::time::timeout(Duration::from_millis(50), handle).await;
        assert!(result.is_ok(), "Future timed out");
    }

    #[tokio::test]
    async fn wait_until_drop_removes_waker() {
        let initial_state = SharedState {
            condition_met: false,
            waker_set: WakerSet::new(),
        };
        let lock = Arc::new(SpinLockIrq::<_, MockCpuOps>::new(initial_state));
        let lock_clone = lock.clone();

        let wait_future = wait_until(
            lock.clone(),
            |state| &mut state.waker_set,
            |state| if state.condition_met { Some(()) } else { None },
        );

        let handle = tokio::spawn(async {
            // Poll the future once to register the waker, then drop it.
            let _ = tokio::time::timeout(Duration::from_millis(1), wait_future).await;
        });

        // Wait for the spawned task to complete.
        handle.await.unwrap();

        // Check that the waker has been removed from the waker set.
        let state = lock_clone.lock_save_irq();
        assert!(state.waker_set.waiters.is_empty());
    }
}
