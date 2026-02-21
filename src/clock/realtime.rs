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

pub fn set_date(duration: Duration) {
    if let Some(now) = now() {
        let mut epoch_info = EPOCH_DURATION.lock_save_irq();
        *epoch_info = Some((duration, now));
    }
}

// Represents a known duration since the epoch at the assoicated instant.
static EPOCH_DURATION: SpinLock<Option<(Duration, Instant)>> = SpinLock::new(None);

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
