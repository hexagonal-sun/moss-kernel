use crate::{
    drivers::timer::{Instant, now, uptime},
    sync::SpinLock,
};
use core::time::Duration;

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

// Represents a known duration since the epoch at the assoicated instant.
static EPOCH_DURATION: SpinLock<Option<(Duration, Instant)>> = SpinLock::new(None);
