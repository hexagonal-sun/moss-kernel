use super::timespec::TimeSpec;
use crate::memory::uaccess::{UserCopyable, copy_to_user};
use core::time::Duration;
use libkernel::{error::Result, memory::address::TUA};

#[derive(Copy, Clone)]
pub struct TimeZone {
    _tz_minuteswest: i32,
    _tz_dsttime: i32,
}

unsafe impl UserCopyable for TimeZone {}

pub async fn sys_gettimeofday(tv: TUA<TimeSpec>, tz: TUA<TimeZone>) -> Result<usize> {
    let time: TimeSpec = Duration::new(0, 0).into();

    copy_to_user(tv, time).await?;

    if !tz.is_null() {
        copy_to_user(
            tz,
            TimeZone {
                _tz_minuteswest: 0,
                _tz_dsttime: 0,
            },
        )
        .await?;
    }

    Ok(0)
}
