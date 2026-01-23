use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

use crate::{
    memory::uaccess::{UserCopyable, copy_from_user, copy_to_user},
    process::thread_group::{TG_LIST, Tgid},
    sched::current::current_task,
};

use super::pid::PidT;

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
#[allow(clippy::upper_case_acronyms)]
pub enum RlimitId {
    CPU = 0,
    FSIZE = 1,
    DATA = 2,
    STACK = 3,
    CORE = 4,
    RSS = 5,
    NPROC = 6,
    NOFILE = 7,
    MEMLOCK = 8,
    AS = 9,
    LOCKS = 10,
    SIGPENDING = 11,
    MSGQUEUE = 12,
    NICE = 13,
    RPRIO = 14,
    RTTIME = 15,
    NLIMITS = 16,
}

impl RlimitId {
    pub const fn as_usize(self) -> usize {
        self as usize
    }
}

impl TryFrom<u32> for RlimitId {
    type Error = KernelError;

    fn try_from(value: u32) -> Result<Self> {
        if value < RlimitId::NLIMITS as u32 {
            // Safety: We've checked that the value is within the valid range of
            // variants.
            Ok(unsafe { core::mem::transmute::<u32, RlimitId>(value) })
        } else {
            Err(KernelError::InvalidValue)
        }
    }
}

pub const RLIM_INFINITY: u64 = u64::MAX;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RLimit {
    pub rlim_cur: u64, // The current (soft) limit
    pub rlim_max: u64, // The hard limit
}

unsafe impl UserCopyable for RLimit {}

impl Default for RLimit {
    /// A sensible default for a limit: 0 for both soft and hard.
    fn default() -> Self {
        Self {
            rlim_cur: 0,
            rlim_max: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResourceLimits {
    limits: [RLimit; RlimitId::NLIMITS.as_usize()],
}

impl Default for ResourceLimits {
    /// These are the default resource limits for the init process (PID 1). All
    /// other processes will inherit these unless they are changed. These values
    /// are modeled directly on the Linux kernel's defaults.
    fn default() -> Self {
        let mut limits = [RLimit::default(); RlimitId::NLIMITS.as_usize()];

        limits[RlimitId::CPU.as_usize()] = RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        };
        limits[RlimitId::FSIZE.as_usize()] = RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        };
        limits[RlimitId::DATA.as_usize()] = RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        };
        limits[RlimitId::STACK.as_usize()] = RLimit {
            rlim_cur: 8 * 1024 * 1024,
            rlim_max: RLIM_INFINITY,
        }; // 8MB soft limit
        limits[RlimitId::CORE.as_usize()] = RLimit {
            rlim_cur: 0,
            rlim_max: RLIM_INFINITY,
        }; // Core dumps off by default
        limits[RlimitId::RSS.as_usize()] = RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        };
        limits[RlimitId::NPROC.as_usize()] = RLimit {
            rlim_cur: 4096,
            rlim_max: 8192,
        };
        limits[RlimitId::NOFILE.as_usize()] = RLimit {
            rlim_cur: 1024,
            rlim_max: 4096,
        };
        limits[RlimitId::MEMLOCK.as_usize()] = RLimit {
            rlim_cur: 65536,
            rlim_max: 65536,
        }; // 64KB
        limits[RlimitId::AS.as_usize()] = RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        };
        limits[RlimitId::LOCKS.as_usize()] = RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        };
        limits[RlimitId::SIGPENDING.as_usize()] = RLimit {
            rlim_cur: 4096,
            rlim_max: 4096,
        };
        limits[RlimitId::MSGQUEUE.as_usize()] = RLimit {
            rlim_cur: 819200,
            rlim_max: 819200,
        };
        limits[RlimitId::NICE.as_usize()] = RLimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        limits[RlimitId::RPRIO.as_usize()] = RLimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        limits[RlimitId::RTTIME.as_usize()] = RLimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        };

        Self { limits }
    }
}

impl ResourceLimits {
    pub fn get(&self, id: RlimitId) -> RLimit {
        self.limits[id.as_usize()]
    }

    /// Attempt to set a new resource limit, returning the old value if changed.
    pub fn set(&mut self, id: RlimitId, new_limit: RLimit, is_privileged: bool) -> Result<RLimit> {
        let old_limit = self.get(id);

        // The new soft limit cannot exceed the new hard limit.
        if new_limit.rlim_cur > new_limit.rlim_max {
            return Err(KernelError::InvalidValue);
        }

        // The new hard limit cannot exceed the old hard limit, unless the
        // process is privileged.
        if new_limit.rlim_max > old_limit.rlim_max && !is_privileged {
            return Err(KernelError::NotPermitted);
        }

        // The new values are valid. Commit them.
        self.limits[id.as_usize()] = new_limit;

        Ok(old_limit)
    }
}

pub async fn sys_prlimit64(
    pid: PidT,
    resource: u32,
    new_rlim: TUA<RLimit>,
    old_rlim: TUA<RLimit>,
) -> Result<usize> {
    let resource: RlimitId = resource.try_into()?;

    let task = if pid == 0 {
        current_task().process.clone()
    } else {
        TG_LIST
            .lock_save_irq()
            .get(&Tgid::from_pid_t(pid))
            .and_then(|x| x.upgrade())
            .ok_or(KernelError::NoProcess)?
    };

    let new_limit = if !new_rlim.is_null() {
        Some(copy_from_user(new_rlim).await?)
    } else {
        None
    };

    let old_lim = if let Some(new_limit) = new_limit {
        task.rsrc_lim
            .lock_save_irq()
            .set(resource, new_limit, true)?
    } else {
        task.rsrc_lim.lock_save_irq().get(resource)
    };

    if !old_rlim.is_null() {
        copy_to_user(old_rlim, old_lim).await?;
    }

    Ok(0)
}
