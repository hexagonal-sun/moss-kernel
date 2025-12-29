use core::time::Duration;

use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

use crate::memory::uaccess::{UserCopyable, copy_from_user};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TimeSpec {
    pub tv_sec: i64,
    pub tv_nsec: u64,
}

unsafe impl UserCopyable for TimeSpec {}

impl From<TimeSpec> for Duration {
    fn from(value: TimeSpec) -> Self {
        Duration::new(value.tv_sec as _, value.tv_nsec as _)
    }
}

impl From<Duration> for TimeSpec {
    fn from(value: Duration) -> Self {
        TimeSpec {
            tv_sec: value.as_secs() as _,
            tv_nsec: value.subsec_nanos() as _,
        }
    }
}

impl TimeSpec {
    pub async fn copy_from_user(src: TUA<Self>) -> Result<Self> {
        let timespec = copy_from_user(src).await?;

        // Sanity checking.
        if timespec.tv_nsec > 999_999_999 {
            return Err(KernelError::InvalidValue);
        }

        if timespec.tv_sec < 0 {
            return Err(KernelError::InvalidValue);
        }

        Ok(timespec)
    }
}
