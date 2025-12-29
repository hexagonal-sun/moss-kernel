use super::Driver;
use crate::interrupts::{InterruptDescriptor, InterruptHandler};
use crate::per_cpu;
use crate::sync::OnceLock;
use alloc::{collections::binary_heap::BinaryHeap, sync::Arc};
use core::{
    future::poll_fn,
    ops::{Add, Sub},
    task::{Poll, Waker},
    time::Duration,
};

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
    Preempt,
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

    /// Schedules an interrupt to occur at `when` on *this* CPU. If when is
    /// `None`, timer interrupts should be disabled.
    fn schedule_interrupt(&self, when: Option<Instant>);
}

pub struct SysTimer {
    start_time: Instant,
    driver: Arc<dyn HwTimer>,
}

impl Driver for SysTimer {
    fn name(&self) -> &'static str {
        self.driver.name()
    }
}

impl InterruptHandler for SysTimer {
    fn handle_irq(&self, _desc: InterruptDescriptor) {
        let mut wake_q = WAKEUP_Q.borrow_mut();

        while let Some(next_event) = wake_q.peek() {
            if next_event.when <= self.driver.now() {
                let event = wake_q.pop().unwrap(); // We know it's there from peek()

                match event.what {
                    WakeupKind::Task(waker) => waker.wake(),
                    WakeupKind::Preempt => {
                        // Do nothing, the IRQ return-to-userspace code will
                        // call schedule() for us.
                    }
                }
            } else {
                // The next event is in the future, so we're done.
                break;
            }
        }

        // Always re-arm: either next task/event, or a periodic/preemption tick.
        let next_deadline = wake_q.peek().map(|e| e.when).or_else(|| {
            // fallback: schedule a preemption tick in 50 ms
            // TODO: Remove when feeling more secure about scheduling
            let when = self.driver.now() + Duration::from_millis(50);
            Some(when)
        });

        self.driver.schedule_interrupt(next_deadline);
    }
}

impl SysTimer {
    pub fn uptime(&self) -> Duration {
        self.driver.now() - self.start_time
    }

    fn from_driver(driver: Arc<dyn HwTimer>) -> Self {
        Self {
            start_time: driver.now(),
            driver,
        }
    }

    pub async fn sleep(&self, duration: Duration) -> () {
        let when = self.driver.now() + duration;

        poll_fn(|cx| {
            if self.driver.now() >= when {
                Poll::Ready(())
            } else {
                let mut wakeup_q = WAKEUP_Q.borrow_mut();

                wakeup_q.push(WakeupEvent {
                    when,
                    what: WakeupKind::Task(cx.waker().clone()),
                });

                // After pushing, we must update the hardware timer in case our
                // new event is the earliest one.
                if let Some(next_event) = wakeup_q.peek() {
                    self.driver.schedule_interrupt(Some(next_event.when));
                }

                Poll::Pending
            }
        })
        .await
    }

    /// Schedule a preemption event for the current CPU.
    pub fn schedule_preempt(&self, when: Instant) {
        let mut wake_q = WAKEUP_Q.borrow_mut();

        // Insert the pre-emption event.
        wake_q.push(WakeupEvent {
            when,
            what: WakeupKind::Preempt,
        });

        // Ensure the hardware timer is armed for the earliest event.
        if let Some(next_event) = wake_q.peek() {
            self.driver.schedule_interrupt(Some(next_event.when));
        }
    }

    /// Arms the hardware timer on the current CPU so that the next scheduled
    /// `WakeupEvent` (or the fallback pre-emption tick) will fire.
    /// Secondary CPUs should call this right after they have enabled their
    /// interrupt controller so that they start receiving timer interrupts.
    pub fn kick_current_cpu(&self) {
        let wake_q = WAKEUP_Q.borrow_mut();

        let next_deadline = wake_q.peek().map(|e| e.when).or_else(|| {
            // Fallback: re-use the same 15 ms periodic tick as the primary CPU.
            Some(self.driver.now() + Duration::from_millis(15))
        });

        self.driver.schedule_interrupt(next_deadline);
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

/// Arms the per-CPU hardware timer for the current core.
/// See [`SysTimer::kick_current_cpu`]
pub fn kick_current_cpu() {
    if let Some(timer) = SYS_TIMER.get() {
        timer.kick_current_cpu();
    }
}

/// Arms a pre-emption timer for the running task on this CPU.
/// Called by the scheduler every time it issues a new eligible virtual deadline.
pub fn schedule_preempt(when: Instant) {
    if let Some(timer) = SYS_TIMER.get() {
        timer.schedule_preempt(when);
    }
}

pub fn schedule_force_preempt() {
    // Schedule a preemption event if none are scheduled
    let when = now().unwrap() + Duration::from_millis(5);

    if let Some(next_event) = WAKEUP_Q.borrow().peek()
        && next_event.when <= when
    {
        // An event is already scheduled before our forced preemption
        return;
    }

    schedule_preempt(when);
}

static SYS_TIMER: OnceLock<Arc<SysTimer>> = OnceLock::new();

per_cpu! {
    static WAKEUP_Q: BinaryHeap<WakeupEvent> = BinaryHeap::new;
}
