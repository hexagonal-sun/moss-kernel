use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use core::time::Duration;
use futures::FutureExt;
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

use super::get_or_create_queue;
use super::key::FutexKey;
use super::waiter::WaiterCell;
use crate::drivers::timer::sleep;
use crate::memory::uaccess::{copy_from_user, try_copy_from_user};

/// One decoded wait request: wait on `key` while `*uaddr == val`, wakeable by
/// any wake whose mask overlaps `mask`.
pub struct ParsedWaiter {
    pub key: FutexKey,
    pub uaddr: TUA<u32>,
    pub val: u32,
    pub mask: u64,
}

/// Owns the queue registrations of an in-progress multi-wait, so that
/// cancellation (signal, timeout, fault retry) always unregisters them.
#[derive(Default)]
struct WaitGuard {
    cells: Vec<Arc<WaiterCell>>,
}

impl WaitGuard {
    /// Ready with the index of the first woken waiter, if any.
    ///
    /// Does not re-register the waker: each cell's queue entry already holds
    /// the task's waker from setup, and that waker is assumed stable across
    /// polls (the same assumption the rest of the kernel makes).
    fn poll_woken(&self) -> Poll<usize> {
        match self.cells.iter().position(|cell| cell.is_woken()) {
            Some(idx) => Poll::Ready(idx),
            None => Poll::Pending,
        }
    }

    /// Unregisters every remaining cell. If any wake was consumed (either
    /// before this call or racing with it), returns the lowest such index.
    fn finish(&mut self) -> Option<usize> {
        let mut woken = None;

        for (idx, cell) in self.cells.iter().enumerate() {
            if cell.unregister() && woken.is_none() {
                woken = Some(idx);
            }
        }

        self.cells.clear();
        woken
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        for cell in &self.cells {
            cell.unregister();
        }
    }
}

enum Setup {
    /// All waiters value-checked and enqueued.
    Queued,
    /// A futex word didn't match its expected value.
    Mismatch,
    /// Reading the futex word at this index faulted; fault it in and retry.
    NeedFault(usize),
}

/// Enqueues each waiter, checking its futex value under the queue lock.
///
/// Synchronous; runs inside a single poll so no wake can slip between the
/// value check and registration of any one waiter (the queue lock covers
/// both). Holds at most one queue lock at a time.
fn setup_all(waiters: &[ParsedWaiter], waker: &Waker, guard: &mut WaitGuard) -> Setup {
    for (idx, waiter) in waiters.iter().enumerate() {
        let queue = get_or_create_queue(waiter.key);
        let mut queue_guard = queue.lock_save_irq();

        match try_copy_from_user(waiter.uaddr) {
            Ok(val) => {
                if val != waiter.val {
                    return Setup::Mismatch;
                }

                let cell = WaiterCell::new(waiter.mask);
                let token = queue_guard.register_with_data(waker, cell.clone());
                cell.set_location(queue.clone(), token);
                guard.cells.push(cell);
            }
            Err(_) => return Setup::NeedFault(idx),
        }
    }

    Setup::Queued
}

/// Waits until any of `waiters` is woken, returning its index.
///
/// Linux `futex_wait_multiple` semantics: all waiters are enqueued with their
/// values checked atomically against concurrent wakes; on value mismatch the
/// queued prefix is unwound and, if one of those waiters was already woken,
/// that counts as a successful wake rather than `EAGAIN`.
pub async fn futex_wait_multi(
    waiters: &[ParsedWaiter],
    timeout: Option<Duration>,
) -> Result<usize> {
    loop {
        let mut guard = WaitGuard::default();

        // poll_fn is used purely to obtain the task's waker; setup itself
        // never returns Pending.
        let setup = poll_fn(|cx| Poll::Ready(setup_all(waiters, cx.waker(), &mut guard))).await;

        match setup {
            Setup::NeedFault(idx) => {
                if let Some(woken) = guard.finish() {
                    return Ok(woken);
                }

                copy_from_user(waiters[idx].uaddr).await?;
                continue;
            }
            Setup::Mismatch => {
                return guard.finish().ok_or(KernelError::TryAgain);
            }
            Setup::Queued => {}
        }

        return match timeout {
            None => {
                let woken = poll_fn(|_| guard.poll_woken()).await;
                guard.finish();
                Ok(woken)
            }
            Some(dur) => {
                let mut wait = poll_fn(|_| guard.poll_woken()).fuse();
                let mut sleep_fut = Box::pin(sleep(dur).fuse());

                let woken = futures::select_biased! {
                    idx = wait => Some(idx),
                    _ = sleep_fut => None,
                };

                drop(wait);

                match woken {
                    Some(idx) => {
                        guard.finish();
                        Ok(idx)
                    }
                    // A wake may have landed between the timer firing and us
                    // unregistering; finish() reports it so the wake isn't
                    // lost.
                    None => guard.finish().ok_or(KernelError::TimedOut),
                }
            }
        };
    }
}
