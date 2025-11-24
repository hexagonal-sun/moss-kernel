use core::{
    future::poll_fn,
    ops::{Add, Sub},
    task::{Poll, Waker},
    time::Duration,
};

use alloc::{collections::binary_heap::BinaryHeap, sync::Arc};

use crate::{
    interrupts::{InterruptDescriptor, InterruptHandler},
    sync::{OnceLock, SpinLock},
};

use super::Driver;

pub mod armv8_arch;

/// Represents a fixed point in monotonic time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instant {
    ticks: u64,
    freq: u64,
}

impl Ord for Instant {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.ticks.cmp(&other.ticks)
    }
}

enum WakeupKind {
    ///  This scheduled wake up is for an async task.
    Task(Waker),

    /// This wake up is for the kernel's preemption mechanism.
    _Preempt,
}

struct WakeupEvent {
    when: Instant,
    what: WakeupKind,
}

impl PartialEq for WakeupEvent {
    fn eq(&self, other: &Self) -> bool {
        self.when == other.when
    }
}

impl Eq for WakeupEvent {}

#[allow(clippy::non_canonical_partial_ord_impl)]
impl PartialOrd for WakeupEvent {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.when.cmp(&other.when).reverse())
    }
}

impl Ord for WakeupEvent {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.partial_cmp(other).unwrap()
    }
}

impl Add<Duration> for Instant {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self::Output {
        let secs_tick = rhs.as_secs() * self.freq;
        let nsecs_tick = ((self.freq as u128 * rhs.subsec_nanos() as u128) / 1_000_000_000) as u64;

        Self {
            ticks: self.ticks + secs_tick + nsecs_tick,
            freq: self.freq,
        }
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, rhs: Instant) -> Self::Output {
        debug_assert_eq!(self.freq, rhs.freq);

        let diff_ticks = self.ticks.saturating_sub(rhs.ticks);

        let secs = diff_ticks / self.freq;
        let remaining_ticks = diff_ticks % self.freq;

        let nanos = ((remaining_ticks as u128 * 1_000_000_000) / self.freq as u128) as u32;

        Duration::new(secs, nanos)
    }
}

#[allow(clippy::non_canonical_partial_ord_impl)]
impl PartialOrd for Instant {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.ticks.partial_cmp(&other.ticks)
    }
}

pub trait HwTimer: Send + Sync + Driver {
    /// Return an instant that represents this instant.
    fn now(&self) -> Instant;

    /// Schedules an interrupt to occur at `when`. If when is `None`, timer
    /// interrupts should be disabled.
    fn schedule_interrupt(&self, when: Option<Instant>);
}

pub struct SysTimer {
    start_time: Instant,
    wakeup_q: SpinLock<BinaryHeap<WakeupEvent>>,
    driver: Arc<dyn HwTimer>,
}

impl Driver for SysTimer {
    fn name(&self) -> &'static str {
        self.driver.name()
    }
}

impl InterruptHandler for SysTimer {
    fn handle_irq(&self, _desc: InterruptDescriptor) {
        let mut wake_q = self.wakeup_q.lock_save_irq();

        while let Some(next_event) = wake_q.peek() {
            if next_event.when <= self.driver.now() {
                let event = wake_q.pop().unwrap(); // We know it's there from peek()

                match event.what {
                    WakeupKind::Task(waker) => waker.wake(),
                    WakeupKind::_Preempt => todo!(),
                }
            } else {
                // The next event is in the future, so we're done.
                break;
            }
        }

        // Reschedule based on the new head of the queue.
        self.driver
            .schedule_interrupt(wake_q.peek().map(|e| e.when));
    }
}

impl SysTimer {
    pub fn uptime(&self) -> Duration {
        self.driver.now() - self.start_time
    }

    fn from_driver(driver: Arc<dyn HwTimer>) -> Self {
        Self {
            start_time: driver.now(),
            wakeup_q: SpinLock::new(BinaryHeap::new()),
            driver,
        }
    }

    pub async fn sleep(&self, duration: Duration) -> () {
        let when = self.driver.now() + duration;

        poll_fn(|cx| {
            let mut wake_q = self.wakeup_q.lock_save_irq();

            if self.driver.now() >= when {
                Poll::Ready(())
            } else {
                wake_q.push(WakeupEvent {
                    when,
                    what: WakeupKind::Task(cx.waker().clone()),
                });

                // After pushing, we must update the hardware timer in case our
                // new event is the earliest one.
                if let Some(next_event) = wake_q.peek() {
                    self.driver.schedule_interrupt(Some(next_event.when));
                }

                Poll::Pending
            }
        })
        .await
    }
}

/// Convenience function for obtaining the current system time. If no
/// `SYS_TIMER` has been setup by the kernel yet, returns a zero duration.
pub fn uptime() -> Duration {
    SYS_TIMER
        .get()
        .map(|timer| timer.uptime())
        .unwrap_or(Duration::ZERO)
}

/// Returns the current instant, if the system timer has been initialised.
pub fn now() -> Option<Instant> {
    SYS_TIMER.get().map(|timer| timer.driver.now())
}

/// Puts the current task to sleep for `duration`. If no timer driver has yet
/// been loaded, the funtion returns without sleeping.
pub async fn sleep(duration: Duration) {
    // A sleep of zero duration returns now.
    if duration.is_zero() {
        return;
    }

    if let Some(timer) = SYS_TIMER.get() {
        timer.sleep(duration).await
    }
}

static SYS_TIMER: OnceLock<Arc<SysTimer>> = OnceLock::new();
