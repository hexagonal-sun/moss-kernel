use crate::clock::ClockId;
use crate::clock::realtime::set_date;
use crate::clock::timespec::TimeSpec;
use crate::memory::uaccess::copy_from_user;
use libkernel::error::KernelError;
use libkernel::memory::address::TUA;

pub async fn sys_clock_settime(
    clockid: i32,
    time_spec: TUA<TimeSpec>,
) -> libkernel::error::Result<usize> {
    let time_spec = copy_from_user(time_spec).await?;
    if time_spec.tv_sec < 0 || time_spec.tv_nsec >= 1_000_000_000 {
        return Err(KernelError::InvalidValue);
    }
    match ClockId::try_from(clockid).map_err(|_| KernelError::InvalidValue)? {
        ClockId::Monotonic | ClockId::MonotonicCoarse | ClockId::MonotonicRaw => {
            // Monotonic clock cannot be set
            Err(KernelError::InvalidValue)
        }
        ClockId::Realtime => {
            set_date(time_spec.into());
            Ok(0)
        }
        _ => Err(KernelError::NotSupported),
    }
}
