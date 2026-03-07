//! Real-time clock (RTC) drivers.
//!
//! RTCs often differ in how they represent time, so the idea is to return a [`Duration`] since the Unix epoch,
//! with each driver responsible for converting/handling hardware bugs.

pub mod pl031;

use crate::sync::OnceLock;
use alloc::sync::Arc;
use core::time::Duration;

pub trait Rtc: Send + Sync {
    /// Gets the current RTC time as a `Duration` since the Unix epoch.
    fn time(&self) -> Option<Duration>;

    /// Sets the RTC time. The provided `Duration` should represent the time since the Unix epoch.
    #[expect(unused)]
    fn set_time(&mut self, time: Duration) -> libkernel::error::Result<()>;
}

pub static RTC_DRIVER: OnceLock<Arc<dyn Rtc>> = OnceLock::new();

pub fn get_rtc() -> Option<&'static Arc<dyn Rtc>> {
    RTC_DRIVER.get()
}

fn set_rtc_driver(driver: Arc<dyn Rtc>) -> bool {
    RTC_DRIVER.set(driver).is_ok()
}
