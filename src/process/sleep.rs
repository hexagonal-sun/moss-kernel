use libkernel::{error::Result, memory::address::TUA};

use crate::{clock::timespec::TimeSpec, drivers::timer::sleep};

pub async fn sys_nanosleep(rqtp: TUA<TimeSpec>, _rmtp: TUA<TimeSpec>) -> Result<usize> {
    let timespec = TimeSpec::copy_from_user(rqtp).await?;

    sleep(timespec.into()).await;

    Ok(0)
}

pub async fn sys_clock_nanosleep(
    _clock_id: i32,
    rqtp: TUA<TimeSpec>,
    _rmtp: TUA<TimeSpec>,
) -> Result<usize> {
    let timespec = TimeSpec::copy_from_user(rqtp).await?;

    sleep(timespec.into()).await;

    Ok(0)
}
