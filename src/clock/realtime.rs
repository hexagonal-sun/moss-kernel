use crate::{
    drivers::timer::{Instant, now, uptime},
    sync::{OnceLock, SpinLock},
};
use core::future::poll_fn;
use core::task::Poll;
use core::time::Duration;
use libkernel::sync::waker_set::WakerSet;

// Return a duration from the epoch.
pub fn date() -> Duration {
    let epoch_info = *EPOCH_DURATION.lock_save_irq();

    if let Some(ep_info) = epoch_info
        && let Some(now) = now()
    {
        let duraton_since_ep_info = now - ep_info.1;
        ep_info.0 + duraton_since_ep_info
    } else {
        uptime()
    }
}

pub fn set_date(duration: Duration) {
    if let Some(now) = now() {
        let mut epoch_info = EPOCH_DURATION.lock_save_irq();
        *epoch_info = Some((duration, now));
    }

    // The realtime clock was stepped; wake anyone sleeping against an absolute
    // realtime deadline so they can re-evaluate (and re-arm) against the new
    // wall time.
    let mut waiters = clock_set_waiters().lock_save_irq();
    *CLOCK_SET_GEN.lock_save_irq() += 1;
    waiters.wake_all();
}

// Represents a known duration since the epoch at the associated instant.
static EPOCH_DURATION: SpinLock<Option<(Duration, Instant)>> = SpinLock::new(None);

/// Tasks waiting to be notified when the realtime clock is stepped.
static CLOCK_SET_WAITERS: OnceLock<SpinLock<WakerSet>> = OnceLock::new();

fn clock_set_waiters() -> &'static SpinLock<WakerSet> {
    CLOCK_SET_WAITERS.get_or_init(|| SpinLock::new(WakerSet::new()))
}

/// Bumped on every realtime clock step, so a waiter can detect a step that
/// happened between checking the clock and parking (closing the lost-wakeup
/// race).
static CLOCK_SET_GEN: SpinLock<u64> = SpinLock::new(0);

/// The current clock-set generation. Sample this before reading the clock, and
/// pass it to [`clock_was_set_since`] to wait for the next step.
pub fn clock_set_generation() -> u64 {
    *CLOCK_SET_GEN.lock_save_irq()
}

/// Removes its waker from [`clock_set_waiters`] when dropped, so a caller that
/// abandons the wait (e.g. a wrapping `select!` that exits via a timer) does
/// not leave a stale registration lingering until the next clock step.
struct ClockSetRegistration {
    token: Option<u64>,
}

impl Drop for ClockSetRegistration {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            clock_set_waiters().lock_save_irq().remove(token);
        }
    }
}

/// Resolves once the realtime clock is stepped after `generation` was sampled.
/// If a step already happened since `generation`, returns immediately.
pub async fn clock_was_set_since(generation: u64) {
    let mut registration = ClockSetRegistration { token: None };

    poll_fn(|cx| {
        // Register before re-checking the generation so a step that races our
        // poll cannot be missed.
        let mut waiters = clock_set_waiters().lock_save_irq();

        if *CLOCK_SET_GEN.lock_save_irq() != generation {
            return Poll::Ready(());
        }

        if registration.token.is_none() {
            registration.token = Some(waiters.register(cx.waker()));
        }

        Poll::Pending
    })
    .await;

    // Reaching here means the generation changed and our waker was already
    // consumed by `wake_all`; forget the token so drop does not try to remove
    // an id that may have been reused.
    registration.token = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use moss_macros::ktest;

    #[ktest]
    fn test_date_and_set_date() {
        let initial_date = date();
        let new_date = Duration::from_secs(1_000_000);
        set_date(new_date);
        let updated_date = date();
        assert_ne!(
            initial_date, updated_date,
            "Date should change after set_date"
        );
        assert!(
            updated_date >= new_date,
            "Updated date should be at least the new date set"
        );
    }
}
