use core::convert::Infallible;

use crate::process::thread_group::Sid;
use crate::{
    memory::uaccess::{UserCopyable, copy_to_user},
    sched::syscall_ctx::ProcessCtx,
};
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
    proc::{
        caps::{Capabilities, CapabilitiesFlags},
        ids::{Gid, Uid},
    },
};

unsafe impl UserCopyable for Uid {}
unsafe impl UserCopyable for Gid {}

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    uid: Uid,
    euid: Uid,
    suid: Uid,
    gid: Gid,
    egid: Gid,
    sgid: Gid,
    pub(super) caps: Capabilities,
}

impl Credentials {
    pub fn new_root() -> Self {
        Self {
            uid: Uid::new_root(),
            euid: Uid::new_root(),
            suid: Uid::new_root(),
            gid: Gid::new_root_group(),
            egid: Gid::new_root_group(),
            sgid: Gid::new_root_group(),
            caps: Capabilities::new_root(),
        }
    }

    pub fn uid(&self) -> Uid {
        self.uid
    }

    pub fn euid(&self) -> Uid {
        self.euid
    }

    pub fn suid(&self) -> Uid {
        self.suid
    }

    pub fn gid(&self) -> Gid {
        self.gid
    }

    pub fn egid(&self) -> Gid {
        self.egid
    }

    pub fn sgid(&self) -> Gid {
        self.sgid
    }

    pub fn caps(&self) -> Capabilities {
        self.caps
    }
}

pub fn sys_getuid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let uid: u32 = ctx.shared().creds.lock_save_irq().uid().into();

    Ok(uid as _)
}

pub fn sys_geteuid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let uid: u32 = ctx.shared().creds.lock_save_irq().euid().into();

    Ok(uid as _)
}

pub fn sys_getgid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let gid: u32 = ctx.shared().creds.lock_save_irq().gid().into();

    Ok(gid as _)
}

pub fn sys_getegid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let gid: u32 = ctx.shared().creds.lock_save_irq().egid().into();

    Ok(gid as _)
}

pub fn sys_setuid(ctx: &ProcessCtx, uid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let new_uid = Uid::new(uid as u32);

    if creds.caps.is_capable(CapabilitiesFlags::CAP_SETUID) {
        creds.uid = new_uid;
        creds.euid = new_uid;
        creds.suid = new_uid;
    } else {
        if new_uid == creds.uid || new_uid == creds.suid {
            creds.euid = new_uid;
        } else {
            return Err(KernelError::NotPermitted);
        }
    }

    Ok(0)
}

pub fn sys_setgid(ctx: &ProcessCtx, gid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let new_gid = Gid::new(gid as u32);

    if creds.caps.is_capable(CapabilitiesFlags::CAP_SETGID) {
        creds.gid = new_gid;
        creds.egid = new_gid;
        creds.sgid = new_gid;
    } else {
        if new_gid == creds.gid || new_gid == creds.sgid {
            creds.egid = new_gid;
        } else {
            return Err(KernelError::NotPermitted);
        }
    }

    Ok(0)
}

pub fn sys_setreuid(ctx: &ProcessCtx, ruid: usize, euid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let new_ruid = if ruid == usize::MAX {
        creds.uid
    } else {
        Uid::new(ruid as u32)
    };
    let new_euid = if euid == usize::MAX {
        creds.euid
    } else {
        Uid::new(euid as u32)
    };

    let capable = creds.caps.is_capable(CapabilitiesFlags::CAP_SETUID);

    if !capable {
        if ruid != usize::MAX
            && new_ruid != creds.uid
            && new_ruid != creds.euid
            && new_ruid != creds.suid
        {
            return Err(KernelError::NotPermitted);
        }
        if euid != usize::MAX
            && new_euid != creds.uid
            && new_euid != creds.euid
            && new_euid != creds.suid
        {
            return Err(KernelError::NotPermitted);
        }
    }

    if ruid != usize::MAX || (euid != usize::MAX && new_euid != creds.uid) {
        creds.suid = new_euid;
    }

    creds.uid = new_ruid;
    creds.euid = new_euid;

    Ok(0)
}

pub fn sys_setregid(ctx: &ProcessCtx, rgid: usize, egid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let new_rgid = if rgid == usize::MAX {
        creds.gid
    } else {
        Gid::new(rgid as u32)
    };
    let new_egid = if egid == usize::MAX {
        creds.egid
    } else {
        Gid::new(egid as u32)
    };

    let capable = creds.caps.is_capable(CapabilitiesFlags::CAP_SETGID);

    if !capable {
        if rgid != usize::MAX
            && new_rgid != creds.gid
            && new_rgid != creds.egid
            && new_rgid != creds.sgid
        {
            return Err(KernelError::NotPermitted);
        }
        if egid != usize::MAX
            && new_egid != creds.gid
            && new_egid != creds.egid
            && new_egid != creds.sgid
        {
            return Err(KernelError::NotPermitted);
        }
    }

    if rgid != usize::MAX || (egid != usize::MAX && new_egid != creds.gid) {
        creds.sgid = new_egid;
    }

    creds.gid = new_rgid;
    creds.egid = new_egid;

    Ok(0)
}

pub fn sys_setresuid(ctx: &ProcessCtx, ruid: usize, euid: usize, suid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let new_ruid = if ruid == usize::MAX {
        creds.uid
    } else {
        Uid::new(ruid as u32)
    };
    let new_euid = if euid == usize::MAX {
        creds.euid
    } else {
        Uid::new(euid as u32)
    };
    let new_suid = if suid == usize::MAX {
        creds.suid
    } else {
        Uid::new(suid as u32)
    };

    let capable = creds.caps.is_capable(CapabilitiesFlags::CAP_SETUID);

    if !capable {
        if ruid != usize::MAX
            && new_ruid != creds.uid
            && new_ruid != creds.euid
            && new_ruid != creds.suid
        {
            return Err(KernelError::NotPermitted);
        }
        if euid != usize::MAX
            && new_euid != creds.uid
            && new_euid != creds.euid
            && new_euid != creds.suid
        {
            return Err(KernelError::NotPermitted);
        }
        if suid != usize::MAX
            && new_suid != creds.uid
            && new_suid != creds.euid
            && new_suid != creds.suid
        {
            return Err(KernelError::NotPermitted);
        }
    }

    creds.uid = new_ruid;
    creds.euid = new_euid;
    creds.suid = new_suid;

    Ok(0)
}

pub fn sys_setresgid(ctx: &ProcessCtx, rgid: usize, egid: usize, sgid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let new_rgid = if rgid == usize::MAX {
        creds.gid
    } else {
        Gid::new(rgid as u32)
    };
    let new_egid = if egid == usize::MAX {
        creds.egid
    } else {
        Gid::new(egid as u32)
    };
    let new_sgid = if sgid == usize::MAX {
        creds.sgid
    } else {
        Gid::new(sgid as u32)
    };

    let capable = creds.caps.is_capable(CapabilitiesFlags::CAP_SETGID);

    if !capable {
        if rgid != usize::MAX
            && new_rgid != creds.gid
            && new_rgid != creds.egid
            && new_rgid != creds.sgid
        {
            return Err(KernelError::NotPermitted);
        }
        if egid != usize::MAX
            && new_egid != creds.gid
            && new_egid != creds.egid
            && new_egid != creds.sgid
        {
            return Err(KernelError::NotPermitted);
        }
        if sgid != usize::MAX
            && new_sgid != creds.gid
            && new_sgid != creds.egid
            && new_sgid != creds.sgid
        {
            return Err(KernelError::NotPermitted);
        }
    }

    creds.gid = new_rgid;
    creds.egid = new_egid;
    creds.sgid = new_sgid;

    Ok(0)
}

pub fn sys_setfsuid(ctx: &ProcessCtx, _new_id: usize) -> core::result::Result<usize, Infallible> {
    // Return the uid.  This syscall is deprecated.
    sys_getuid(ctx)
}

pub fn sys_setfsgid(ctx: &ProcessCtx, _new_id: usize) -> core::result::Result<usize, Infallible> {
    // Return the gid. This syscall is deprecated.
    sys_getgid(ctx)
}

pub fn sys_gettid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let tid: u32 = ctx.shared().tid.0;

    Ok(tid as _)
}

pub async fn sys_getresuid(
    ctx: &ProcessCtx,
    ruid: TUA<Uid>,
    euid: TUA<Uid>,
    suid: TUA<Uid>,
) -> Result<usize> {
    let creds = ctx.shared().creds.lock_save_irq().clone();

    copy_to_user(ruid, creds.uid).await?;
    copy_to_user(euid, creds.euid).await?;
    copy_to_user(suid, creds.suid).await?;

    Ok(0)
}

pub async fn sys_getresgid(
    ctx: &ProcessCtx,
    rgid: TUA<Gid>,
    egid: TUA<Gid>,
    sgid: TUA<Gid>,
) -> Result<usize> {
    let creds = ctx.shared().creds.lock_save_irq().clone();

    copy_to_user(rgid, creds.gid).await?;
    copy_to_user(egid, creds.egid).await?;
    copy_to_user(sgid, creds.sgid).await?;

    Ok(0)
}

pub async fn sys_getsid(ctx: &ProcessCtx) -> Result<usize> {
    let sid: u32 = ctx.shared().process.sid.lock_save_irq().value();

    Ok(sid as _)
}

pub async fn sys_setsid(ctx: &ProcessCtx) -> Result<usize> {
    let process = ctx.shared().process.clone();

    let new_sid = process.tgid.value();
    *process.sid.lock_save_irq() = Sid(new_sid);

    Ok(new_sid as _)
}
