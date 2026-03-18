use crate::clock::timespec::TimeSpec;
use crate::drivers::timer::{Instant, now, uptime};
use crate::memory::uaccess::{UserCopyable, copy_from_user, copy_to_user};
use crate::process::thread_group::signal::SigId;
use crate::process::{ITimer, Task, TaskDescriptor, find_task_by_descriptor};
use crate::sched::syscall_ctx::ProcessCtx;
use core::time::Duration;
use libkernel::memory::address::TUA;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ITimerType {
    Real = 0,
    Virtual = 1,
    Prof = 2,
}

impl TryFrom<i32> for ITimerType {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Real),
            1 => Ok(Self::Virtual),
            2 => Ok(Self::Prof),
            _ => Err(()),
        }
    }
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct ITimerVal {
    it_interval: TimeSpec,
    it_value: TimeSpec,
}

impl ITimerVal {
    fn is_disabled(&self) -> bool {
        self.it_value.tv_sec == 0 && self.it_value.tv_nsec == 0
    }

    fn is_oneshot(&self) -> bool {
        self.it_interval.tv_sec == 0 && self.it_interval.tv_nsec == 0
    }
}

impl Default for ITimerVal {
    fn default() -> Self {
        Self {
            it_interval: TimeSpec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: TimeSpec {
                tv_sec: 0,
                tv_nsec: 0,
            },
        }
    }
}

unsafe impl UserCopyable for ITimerVal {}

pub fn itimer_irq_handler(desc: TaskDescriptor, ty: ITimerType) -> Option<Instant> {
    let task = find_task_by_descriptor(&desc)?;
    match ty {
        ITimerType::Real => {
            task.process.deliver_signal(SigId::SIGALRM);
            let mut timers = task.i_timers.lock_save_irq();
            if let Some(ref mut timer) = timers.real
                && let Some(interval) = timer.interval
            {
                timer.next = now().unwrap() + interval;
                Some(timer.next)
            } else {
                timers.real = None;
                None
            }
        }
        _ => unimplemented!(),
    }
}

async fn getitimer(current_task: &Task, which: ITimerType) -> libkernel::error::Result<ITimerVal> {
    let now = match which {
        ITimerType::Real => uptime(),
        _ => unimplemented!(),
    };
    Ok(current_task
        .i_timers
        .lock_save_irq()
        .real
        .map(|t| {
            let remaining = Duration::from(t.next) - now;
            let interval = t.interval.unwrap_or_default();
            ITimerVal {
                it_interval: TimeSpec {
                    tv_sec: interval.as_secs() as _,
                    tv_nsec: interval.subsec_nanos() as _,
                },
                it_value: TimeSpec {
                    tv_sec: remaining.as_secs() as _,
                    tv_nsec: remaining.subsec_nanos() as _,
                },
            }
        })
        .unwrap_or_default())
}

/// <https://man7.org/linux/man-pages/man2/getitimer.2.html>
pub async fn sys_getitimer(
    ctx: &ProcessCtx,
    which: i32,
    curr_value: TUA<ITimerVal>,
) -> libkernel::error::Result<usize> {
    let timer_type =
        ITimerType::try_from(which).map_err(|_| libkernel::error::KernelError::InvalidValue)?;
    let value = getitimer(ctx.shared(), timer_type).await?;
    copy_to_user(curr_value, value).await?;
    Ok(0)
}

/// <https://man7.org/linux/man-pages/man2/setitimer.2.html>
pub async fn sys_setitimer(
    ctx: &ProcessCtx,
    which: i32,
    new_value: TUA<ITimerVal>,
    old_value: TUA<ITimerVal>,
) -> libkernel::error::Result<usize> {
    let timer_type =
        ITimerType::try_from(which).map_err(|_| libkernel::error::KernelError::InvalidValue)?;
    if !old_value.is_null() {
        let old_timer = getitimer(ctx.shared(), timer_type).await?;
        copy_to_user(old_value, old_timer).await?;
    }
    let new_timer = copy_from_user(new_value).await?;
    match timer_type {
        ITimerType::Real => {
            let current_task = ctx.shared();
            let mut timers = current_task.i_timers.lock_save_irq();
            let interval = if new_timer.is_oneshot() {
                None
            } else {
                Some(Duration::new(
                    new_timer.it_interval.tv_sec as _,
                    new_timer.it_interval.tv_nsec as _,
                ))
            };
            let next = if new_timer.is_disabled() {
                None
            } else {
                Some(
                    now().unwrap()
                        + Duration::new(
                            new_timer.it_value.tv_sec as _,
                            new_timer.it_value.tv_nsec as _,
                        ),
                )
            };
            if timers.real.is_some() {
                crate::drivers::timer::SYS_TIMER
                    .get()
                    .unwrap()
                    .remove_scheduled_itimer(current_task.descriptor(), ITimerType::Real);
            }
            if let Some(next) = next {
                timers.real = Some(ITimer { interval, next });
                crate::drivers::timer::SYS_TIMER
                    .get()
                    .unwrap()
                    .schedule_itimer(current_task.descriptor(), ITimerType::Real, next);
            }
        }
        _ => unimplemented!(),
    }
    Ok(0)
}
