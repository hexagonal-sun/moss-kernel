use core::sync::atomic::Ordering;
use core::time::Duration;
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

use super::{ClockId, realtime::date, timespec::TimeSpec};
use crate::drivers::timer::{Instant, now};
use crate::sched::current::current_task_shared;
use crate::{drivers::timer::uptime, memory::uaccess::copy_to_user};

pub async fn sys_clock_gettime(clockid: i32, time_spec: TUA<TimeSpec>) -> Result<usize> {
    let time = match ClockId::try_from(clockid).map_err(|_| KernelError::InvalidValue)? {
        ClockId::Monotonic => uptime(),
        ClockId::Realtime => date(),
        ClockId::ProcessCpuTimeId => {
            let task = current_task_shared();
            let total_time = task.process.stime.load(Ordering::Relaxed) as u64
                + task.process.utime.load(Ordering::Relaxed) as u64;
            let last_update = Instant::from_user_normalized(
                task.process.last_account.load(Ordering::Relaxed) as u64,
            );
            let now = now().unwrap();
            let delta = now - last_update;
            Duration::from(Instant::from_user_normalized(total_time)) + delta
        }
        ClockId::ThreadCpuTimeId => {
            let task = current_task_shared();
            let total_time = task.stime.load(Ordering::Relaxed) as u64
                + task.utime.load(Ordering::Relaxed) as u64;
            let last_update =
                Instant::from_user_normalized(task.last_account.load(Ordering::Relaxed) as u64);
            let now = now().unwrap();
            let delta = now - last_update;
            Duration::from(Instant::from_user_normalized(total_time)) + delta
        }
        _ => return Err(KernelError::InvalidValue),
    };

    copy_to_user(time_spec, time.into()).await?;

    Ok(0)
}
