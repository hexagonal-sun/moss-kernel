use super::timespec::TimeSpec;
use crate::clock::realtime::{date, set_date};
use crate::memory::uaccess::{UserCopyable, copy_from_user, copy_to_user};
use core::time::Duration;
use libkernel::{error::Result, memory::address::TUA};

#[derive(Copy, Clone)]
pub struct TimeZone {
    _tz_minuteswest: i32,
    _tz_dsttime: i32,
}

unsafe impl UserCopyable for TimeZone {}

pub async fn sys_gettimeofday(tv: TUA<TimeSpec>, tz: TUA<TimeZone>) -> Result<usize> {
    let time: TimeSpec = date().into();

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

pub async fn sys_settimeofday(tv: TUA<TimeSpec>, _tz: TUA<TimeZone>) -> Result<usize> {
    let time: TimeSpec = copy_from_user(tv).await?;
    let duration: Duration = time.into();
    set_date(duration);
    Ok(0)
}
