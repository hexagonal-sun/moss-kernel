use super::spinlock::SpinLockIrq;
use super::waker_set::WakerSet;
use crate::CpuOps;
use alloc::sync::Arc;

/// The type of wakeup that should occur after a state update.
pub enum WakeupType {
    None,
    One,
    All,
}

struct CondVarInner<S> {
    state: S,
    wakers: WakerSet,
}

impl<S> CondVarInner<S> {
    fn new(initial_state: S) -> Self {
        Self {
            state: initial_state,
            wakers: WakerSet::new(),
        }
    }
}

/// A condvar for managing asynchronous tasks that need to sleep while sharing
/// state.
///
/// This structure is thread-safe and allows registering wakers, which can be
/// woken selectively or all at once. It is "drop-aware": if a future that
/// registered a waker is dropped, its waker is automatically de-registered.
pub struct CondVar<S, C: CpuOps> {
    inner: Arc<SpinLockIrq<CondVarInner<S>, C>>,
}

impl<S, C: CpuOps> CondVar<S, C> {
    /// Creates a new, empty wait queue, initialized with state `initial_state`.
    pub fn new(initial_state: S) -> Self {
        Self {
            inner: Arc::new(SpinLockIrq::new(CondVarInner::new(initial_state))),
        }
    }

    /// Updates the internal state by calling `updater`.
    ///
    /// The `updater` closure should return the kind of wakeup to perform on the
    /// condvar after performing the update.
    pub fn update(&self, updater: impl FnOnce(&mut S) -> WakeupType) {
        let mut inner = self.inner.lock_save_irq();

        match updater(&mut inner.state) {
            WakeupType::None => (),
            WakeupType::One => {
                inner.wakers.wake_one();
            }
            WakeupType::All => inner.wakers.wake_all(),
        }
    }

    /// Creates a future that waits on the queue until a condition on the
    /// internal state is met, returning a value `T` when finished waiting.
    ///
    /// # Arguments
    /// * `predicate`: A closure that checks the condition in the underlying
    ///   state. It should return `None` to continue waiting, or `Some(T)` to
    ///   stop waiting and yield T to the caller.
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub fn wait_until<T, F>(&self, predicate: F) -> impl Future<Output = T> + use<T, S, C, F>
    where
        F: Fn(&mut S) -> Option<T>,
    {
        super::waker_set::wait_until(
            self.inner.clone(),
            |inner| &mut inner.wakers,
            move |inner| predicate(&mut inner.state),
        )
    }
}

impl<S, C: CpuOps> Clone for CondVar<S, C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[cfg(test)]
mod condvar_tests {
    use crate::test::MockCpuOps;

    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct TestState {
        counter: u32,
    }

    #[tokio::test]
    async fn wait_and_wake_one() {
        let condvar = CondVar::<_, MockCpuOps>::new(TestState { counter: 0 });
        let condvar_clone = condvar.clone();

        let handle = tokio::spawn(async move {
            condvar
                .wait_until(|state| {
                    if state.counter == 1 {
                        Some("Condition Met".to_string())
                    } else {
                        None
                    }
                })
                .await
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!handle.is_finished(), "Future finished prematurely");

        condvar_clone.update(|state| {
            state.counter += 1;
            WakeupType::One
        });

        let result = tokio::time::timeout(Duration::from_millis(50), handle)
            .await
            .expect("Future timed out")
            .unwrap();

        assert_eq!(result, "Condition Met");
    }

    #[tokio::test]
    async fn test_wait_and_wake_all() {
        let condvar = Arc::new(CondVar::<_, MockCpuOps>::new(TestState { counter: 0 }));
        let completion_count = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();

        // Spawn three tasks that wait for the counter to reach 5.
        for _ in 0..3 {
            let condvar_clone = condvar.clone();
            let completion_count_clone = completion_count.clone();
            let handle = tokio::spawn(async move {
                let result = condvar_clone
                    .wait_until(|state| {
                        if state.counter >= 5 {
                            Some(state.counter)
                        } else {
                            None
                        }
                    })
                    .await;
                completion_count_clone.fetch_add(1, Ordering::SeqCst);
                result
            });
            handles.push(handle);
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(completion_count.load(Ordering::SeqCst), 0);

        condvar.update(|state| {
            state.counter = 5;
            WakeupType::All
        });

        for handle in handles {
            let result = handle.await.unwrap();
            assert_eq!(result, 5);
        }

        assert_eq!(completion_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_predicate_already_true() {
        let condvar = CondVar::<_, MockCpuOps>::new(TestState { counter: 10 });

        // This future should complete immediately without pending.
        let result = condvar
            .wait_until(|state| {
                if state.counter == 10 {
                    Some("Already True")
                } else {
                    None
                }
            })
            .await;

        assert_eq!(result, "Already True");
    }

    #[tokio::test]
    async fn test_update_with_no_wakeup() {
        let condvar = CondVar::<_, MockCpuOps>::new(TestState { counter: 0 });

        let handle = {
            let condvar = condvar.clone();
            tokio::spawn(async move {
                condvar
                    .wait_until(|state| if state.counter == 1 { Some(()) } else { None })
                    .await
            })
        };

        tokio::time::sleep(Duration::from_millis(10)).await;

        condvar.update(|state| {
            state.counter = 1;
            WakeupType::None
        });

        // Give some time to see if the future completes (it shouldn't).
        tokio::time::sleep(Duration::from_millis(20)).await;

        // The future should still be pending.
        assert!(!handle.is_finished());

        // Now, perform a wakeup to allow the test to clean up.
        condvar.update(|_| WakeupType::One);
        handle.await.unwrap();
    }
}
