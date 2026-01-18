use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

use crate::{drivers::timer::uptime, memory::uaccess::copy_to_user};

use super::{realtime::date, timespec::TimeSpec, ClockId};

pub async fn sys_clock_gettime(clockid: i32, time_spec: TUA<TimeSpec>) -> Result<usize> {
    let time = match ClockId::try_from(clockid).map_err(|_| KernelError::InvalidValue)? {
        ClockId::Monotonic => uptime(),
        ClockId::Realtime => date(),
        _ => return Err(KernelError::InvalidValue),
    };

    copy_to_user(time_spec, time.into()).await?;

    Ok(0)
}
