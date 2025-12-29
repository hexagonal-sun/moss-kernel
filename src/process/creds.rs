use core::convert::Infallible;

use crate::{
    memory::uaccess::{UserCopyable, copy_to_user},
    sched::current_task,
};
use libkernel::{
    error::Result,
    memory::address::TUA,
    proc::{
        caps::Capabilities,
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

pub fn sys_getuid() -> core::result::Result<usize, Infallible> {
    let uid: u32 = current_task().creds.lock_save_irq().uid().into();

    Ok(uid as _)
}

pub fn sys_geteuid() -> core::result::Result<usize, Infallible> {
    let uid: u32 = current_task().creds.lock_save_irq().euid().into();

    Ok(uid as _)
}

pub fn sys_getgid() -> core::result::Result<usize, Infallible> {
    let gid: u32 = current_task().creds.lock_save_irq().gid().into();

    Ok(gid as _)
}

pub fn sys_getegid() -> core::result::Result<usize, Infallible> {
    let gid: u32 = current_task().creds.lock_save_irq().egid().into();

    Ok(gid as _)
}

pub fn sys_gettid() -> core::result::Result<usize, Infallible> {
    let tid: u32 = current_task().tid.0;

    Ok(tid as _)
}

pub async fn sys_getresuid(ruid: TUA<Uid>, euid: TUA<Uid>, suid: TUA<Uid>) -> Result<usize> {
    let task = current_task();
    let creds = task.creds.lock_save_irq().clone();

    copy_to_user(ruid, creds.uid).await?;
    copy_to_user(euid, creds.euid).await?;
    copy_to_user(suid, creds.suid).await?;

    Ok(0)
}

pub async fn sys_getresgid(rgid: TUA<Gid>, egid: TUA<Gid>, sgid: TUA<Gid>) -> Result<usize> {
    let task = current_task();
    let creds = task.creds.lock_save_irq().clone();

    copy_to_user(rgid, creds.gid).await?;
    copy_to_user(egid, creds.egid).await?;
    copy_to_user(sgid, creds.sgid).await?;

    Ok(0)
}
