pub mod realtime;
pub mod syscalls;
pub mod timer;
pub mod timespec;

use core::time::Duration;

use futures::FutureExt;

use crate::drivers::timer::{sleep, uptime};
use realtime::{clock_set_generation, clock_was_set_since, date};

/// An absolute deadline expressed against a particular clock.
///
/// Keeping the clock alongside the deadline (rather than pre-flattening to a
/// relative duration) lets [`Deadline::sleep`] re-evaluate against the live
/// clock, so a `CLOCK_REALTIME` deadline still fires at the right wall-clock
/// instant even if the clock is stepped (e.g. by `clock_settime`) while a
/// wait is in progress.
#[derive(Clone, Copy)]
pub enum Deadline {
    /// Absolute instant on the monotonic clock (`CLOCK_MONOTONIC`).
    Monotonic(Duration),
    /// Absolute instant on the realtime clock (`CLOCK_REALTIME`).
    Realtime(Duration),
}

impl Deadline {
    /// The clock's current reading.
    fn clock_now(self) -> Duration {
        match self {
            Deadline::Monotonic(_) => uptime(),
            Deadline::Realtime(_) => date(),
        }
    }

    /// The absolute deadline value.
    fn target(self) -> Duration {
        match self {
            Deadline::Monotonic(d) | Deadline::Realtime(d) => d,
        }
    }

    /// Sleeps until this deadline.
    ///
    /// The monotonic clock advances uniformly, so a single relative sleep is
    /// exact. The realtime clock can be stepped by `clock_settime`, so a
    /// realtime wait races the timer against a clock-was-set notification: on
    /// either it re-evaluates the deadline against the live clock and re-arms
    /// if the target has not yet been reached. This retargets an in-progress
    /// wait in both directions across a step.
    pub async fn sleep(self) {
        loop {
            // Sample the clock-set generation *before* reading the clock. A
            // realtime step that lands after this point bumps the generation,
            // so `clock_was_set_since` below fires immediately and we re-loop;
            // sampling after `clock_now()` would leave a window where a step
            // makes `remaining` stale yet goes unnoticed.
            let generation = clock_set_generation();

            let now = self.clock_now();
            let target = self.target();

            if now >= target {
                return;
            }

            let remaining = target - now;

            match self {
                // The monotonic clock never steps, so one relative sleep is
                // exact.
                Deadline::Monotonic(_) => {
                    sleep(remaining).await;
                    return;
                }
                // A realtime step (in either direction) wakes the notifier;
                // loop to re-evaluate against the new wall time.
                Deadline::Realtime(_) => {
                    let mut timer = core::pin::pin!(sleep(remaining).fuse());
                    let mut was_set = core::pin::pin!(clock_was_set_since(generation).fuse());
                    futures::select_biased! {
                        _ = timer => {}
                        _ = was_set => {}
                    }
                }
            }
        }
    }
}

pub enum ClockId {
    Realtime = 0,
    Monotonic = 1,
    ProcessCpuTimeId = 2,
    ThreadCpuTimeId = 3,
    MonotonicRaw = 4,
    RealtimeCoarse = 5,
    MonotonicCoarse = 6,
    BootTime = 7,
    RealtimeAlarm = 8,
    BootTimeAlarm = 9,
    Tai = 11,
}

impl TryFrom<i32> for ClockId {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ClockId::Realtime),
            1 => Ok(ClockId::Monotonic),
            2 => Ok(ClockId::ProcessCpuTimeId),
            3 => Ok(ClockId::ThreadCpuTimeId),
            4 => Ok(ClockId::MonotonicRaw),
            5 => Ok(ClockId::RealtimeCoarse),
            6 => Ok(ClockId::MonotonicCoarse),
            7 => Ok(ClockId::BootTime),
            8 => Ok(ClockId::RealtimeAlarm),
            9 => Ok(ClockId::BootTimeAlarm),
            11 => Ok(ClockId::Tai),
            _ => Err(()),
        }
    }
}
