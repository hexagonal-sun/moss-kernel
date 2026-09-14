use super::user_namespace::UserNamespace;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::sync::atomic::Ordering;

use crate::process::thread_group::Sid;
use crate::{
    memory::uaccess::{UserCopyable, copy_obj_array_from_user, copy_objs_to_user, copy_to_user},
    sched::syscall_ctx::ProcessCtx,
};
use libkernel::{
    error::{KernelError, Result},
    fs::{
        FileType,
        attr::{AccessMode, FileAttr, FilePermissions},
    },
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
    user_ns: Arc<UserNamespace>,
    uid: Uid,
    euid: Uid,
    suid: Uid,
    gid: Gid,
    egid: Gid,
    sgid: Gid,
    fsuid: Uid,
    fsgid: Gid,
    groups: Vec<Gid>,
    pub(super) caps: Capabilities,
}

impl Credentials {
    pub fn new_root() -> Self {
        Self {
            user_ns: UserNamespace::initial(),
            uid: Uid::new_root(),
            euid: Uid::new_root(),
            suid: Uid::new_root(),
            gid: Gid::new_root_group(),
            egid: Gid::new_root_group(),
            sgid: Gid::new_root_group(),
            fsuid: Uid::new_root(),
            fsgid: Gid::new_root_group(),
            groups: Vec::new(),
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

    pub fn user_ns(&self) -> Arc<UserNamespace> {
        self.user_ns.clone()
    }

    /// Capability authority flows towards descendants, never to a parent or
    /// sibling. The owner in the immediate parent controls its child namespace.
    pub fn capable_in(&self, target: &UserNamespace, cap: CapabilitiesFlags) -> bool {
        let mut ns = target;
        loop {
            if self.user_ns.as_ref() == ns {
                return self.caps.is_capable(cap);
            }
            let Some(parent) = ns.parent.as_deref() else {
                return false;
            };
            if parent == self.user_ns.as_ref() && ns.owner == self.euid {
                return true;
            }
            ns = parent;
        }
    }

    pub fn check_capable_in(&self, target: &UserNamespace, cap: CapabilitiesFlags) -> Result<()> {
        if self.capable_in(target, cap) {
            Ok(())
        } else {
            Err(KernelError::NotPermitted)
        }
    }

    pub fn enter_user_ns(&mut self, ns: Arc<UserNamespace>) {
        self.user_ns = ns;
        self.caps = Capabilities::new(
            CapabilitiesFlags::all(),
            CapabilitiesFlags::all(),
            CapabilitiesFlags::empty(),
            CapabilitiesFlags::empty(),
            CapabilitiesFlags::all(),
        );
    }

    pub fn uid_is_root(&self, uid: Uid) -> bool {
        self.user_ns.map_uid_from_kernel(uid) == Some(0)
    }

    /// Capability recalculation for an ordinary executable (file capability
    /// xattrs and set-ID executable transitions are not implemented by MOSS).
    pub fn exec_capabilities(&mut self) {
        let ambient = self.caps.ambient();
        let permitted = if self.uid_is_root(self.uid) || self.uid_is_root(self.euid) {
            self.caps.bounding() | self.caps.inheritable() | ambient
        } else {
            ambient
        };
        let effective = if self.uid_is_root(self.euid) {
            permitted
        } else {
            ambient
        };
        self.caps = Capabilities::new(
            effective,
            permitted,
            self.caps.inheritable(),
            ambient,
            self.caps.bounding(),
        );
        self.suid = self.euid;
        self.sgid = self.egid;
        self.fsuid = self.euid;
        self.fsgid = self.egid;
    }

    /// Linux's capable_wrt_inode_uidgid: capability overrides only apply when
    /// both inode IDs have mappings in the caller's namespace.
    pub fn file_caps(&self, attr: &FileAttr) -> Capabilities {
        if self.user_ns.map_uid_from_kernel(attr.uid).is_some()
            && self.user_ns.map_gid_from_kernel(attr.gid).is_some()
        {
            self.caps
        } else {
            Capabilities::new_empty()
        }
    }

    pub fn map_attr_to_user(&self, attr: &mut FileAttr) {
        attr.uid = Uid::new(self.user_ns.show_uid(attr.uid));
        attr.gid = Gid::new(self.user_ns.show_gid(attr.gid));
    }

    pub fn proc_status(&self, viewer: &UserNamespace) -> alloc::string::String {
        use alloc::format;
        let mut status = format!(
            "Uid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nGroups:\t",
            viewer.show_uid(self.uid),
            viewer.show_uid(self.euid),
            viewer.show_uid(self.suid),
            viewer.show_uid(self.fsuid),
            viewer.show_gid(self.gid),
            viewer.show_gid(self.egid),
            viewer.show_gid(self.sgid),
            viewer.show_gid(self.fsgid)
        );
        for gid in &self.groups {
            status.push_str(&format!("{} ", viewer.show_gid(*gid)));
        }
        status.push_str(&format!("\nCapInh:\t{:016x}\nCapPrm:\t{:016x}\nCapEff:\t{:016x}\nCapBnd:\t{:016x}\nCapAmb:\t{:016x}\n",
            self.caps.inheritable().bits(), self.caps.permitted().bits(), self.caps.effective().bits(), self.caps.bounding().bits(), self.caps.ambient().bits()));
        status
    }

    pub fn fsuid(&self) -> Uid {
        self.fsuid
    }
    pub fn fsgid(&self) -> Gid {
        self.fsgid
    }

    pub fn in_group(&self, gid: Gid) -> bool {
        gid == self.fsgid || self.groups.contains(&gid)
    }

    pub fn check_sticky(&self, parent: &FileAttr, victim: &FileAttr) -> Result<()> {
        if parent.permissions.contains(FilePermissions::S_ISVTX)
            && self.fsuid != parent.uid
            && self.fsuid != victim.uid
        {
            self.file_caps(victim)
                .check_capable(CapabilitiesFlags::CAP_FOWNER)?;
        }
        Ok(())
    }

    pub fn chmod(&self, attr: &mut FileAttr, mode: u16) -> Result<()> {
        if self.fsuid != attr.uid {
            self.file_caps(attr)
                .check_capable(CapabilitiesFlags::CAP_FOWNER)?;
        }
        attr.permissions = FilePermissions::from_bits_truncate(mode);
        if !self.in_group(attr.gid)
            && !self
                .file_caps(attr)
                .is_capable(CapabilitiesFlags::CAP_FSETID)
        {
            attr.permissions.remove(FilePermissions::S_ISGID);
        }
        Ok(())
    }

    pub fn chown(&self, attr: &mut FileAttr, owner: i32, group: i32) -> Result<()> {
        let uid = if owner == -1 {
            attr.uid
        } else {
            self.user_ns.make_uid(owner as u32)?
        };
        let gid = if group == -1 {
            attr.gid
        } else {
            self.user_ns.make_gid(group as u32)?
        };
        if !self
            .file_caps(attr)
            .is_capable(CapabilitiesFlags::CAP_CHOWN)
            && (self.fsuid != attr.uid
                || uid != attr.uid
                || (gid != attr.gid && !self.in_group(gid)))
        {
            return Err(KernelError::NotPermitted);
        }
        attr.uid = uid;
        attr.gid = gid;
        if attr.file_type != FileType::Directory && (owner != -1 || group != -1) {
            attr.permissions.remove(FilePermissions::S_ISUID);
            if attr.permissions.contains(FilePermissions::S_IXGRP) {
                attr.permissions.remove(FilePermissions::S_ISGID);
            }
        }
        Ok(())
    }

    pub fn check_file_access(&self, attr: &FileAttr, mode: AccessMode) -> Result<()> {
        attr.check_access_with_groups(
            self.fsuid,
            self.fsgid,
            &self.groups,
            self.file_caps(attr),
            mode,
        )
    }

    pub async fn check_inode_access(
        &self,
        inode: &dyn libkernel::fs::Inode,
        mode: AccessMode,
    ) -> Result<()> {
        let attr = inode.getattr().await?;
        inode
            .check_access(
                self.fsuid,
                self.fsgid,
                &self.groups,
                self.file_caps(&attr),
                mode,
            )
            .await
    }

    pub fn for_access(&self, effective: bool) -> Self {
        let mut creds = self.clone();
        creds.fsuid = if effective { self.euid } else { self.uid };
        creds.fsgid = if effective { self.egid } else { self.gid };
        if !effective {
            let effective = if self.uid_is_root(self.uid) {
                self.caps.permitted()
            } else {
                CapabilitiesFlags::empty()
            };
            creds.caps = Capabilities::new(
                effective,
                self.caps.permitted(),
                self.caps.inheritable(),
                self.caps.ambient(),
                self.caps.bounding(),
            );
        }
        creds
    }

    fn finish_id_change(&mut self, old: &Self, ctx: &ProcessCtx) {
        let mut permitted = self.caps.permitted();
        let mut effective = self.caps.effective();
        let mut ambient = self.caps.ambient();
        if (old.uid_is_root(old.uid) || old.uid_is_root(old.euid) || old.uid_is_root(old.suid))
            && !(self.uid_is_root(self.uid)
                || self.uid_is_root(self.euid)
                || self.uid_is_root(self.suid))
        {
            permitted = CapabilitiesFlags::empty();
            effective = CapabilitiesFlags::empty();
            ambient = CapabilitiesFlags::empty();
        } else if old.uid_is_root(old.euid) && !self.uid_is_root(self.euid) {
            effective = CapabilitiesFlags::empty();
        } else if !old.uid_is_root(old.euid) && self.uid_is_root(self.euid) {
            effective = permitted;
        }
        let fs_caps = CapabilitiesFlags::CAP_CHOWN
            | CapabilitiesFlags::CAP_DAC_OVERRIDE
            | CapabilitiesFlags::CAP_DAC_READ_SEARCH
            | CapabilitiesFlags::CAP_FOWNER
            | CapabilitiesFlags::CAP_FSETID
            | CapabilitiesFlags::CAP_LINUX_IMMUTABLE
            | CapabilitiesFlags::CAP_MKNOD
            | CapabilitiesFlags::CAP_MAC_OVERRIDE;
        if old.uid_is_root(old.fsuid) && !self.uid_is_root(self.fsuid) {
            effective.remove(fs_caps);
        }
        if !old.uid_is_root(old.fsuid) && self.uid_is_root(self.fsuid) {
            effective.insert(permitted & fs_caps);
        }
        self.caps = Capabilities::new(
            effective,
            permitted,
            self.caps.inheritable(),
            ambient,
            self.caps.bounding(),
        );
        if old.euid != self.euid
            || old.egid != self.egid
            || old.fsuid != self.fsuid
            || old.fsgid != self.fsgid
        {
            ctx.shared().process.dumpable.store(0, Ordering::Release);
        }
    }
}

pub fn sys_getuid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let creds = ctx.shared().creds.lock_save_irq();
    let uid = creds.user_ns.show_uid(creds.uid);

    Ok(uid as _)
}

pub fn sys_geteuid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let creds = ctx.shared().creds.lock_save_irq();
    let uid = creds.user_ns.show_uid(creds.euid);

    Ok(uid as _)
}

pub fn sys_getgid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let creds = ctx.shared().creds.lock_save_irq();
    let gid = creds.user_ns.show_gid(creds.gid);

    Ok(gid as _)
}

pub fn sys_getegid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let creds = ctx.shared().creds.lock_save_irq();
    let gid = creds.user_ns.show_gid(creds.egid);

    Ok(gid as _)
}

pub fn sys_setuid(ctx: &ProcessCtx, uid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let old = creds.clone();
    if uid as u32 == u32::MAX {
        return Err(KernelError::InvalidValue);
    }
    let new_uid = creds.user_ns.make_uid(uid as u32)?;

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

    creds.fsuid = creds.euid;
    creds.finish_id_change(&old, ctx);
    Ok(0)
}

pub fn sys_setgid(ctx: &ProcessCtx, gid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let old = creds.clone();
    if gid as u32 == u32::MAX {
        return Err(KernelError::InvalidValue);
    }
    let new_gid = creds.user_ns.make_gid(gid as u32)?;

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

    creds.fsgid = creds.egid;
    creds.finish_id_change(&old, ctx);
    Ok(0)
}

pub fn sys_setreuid(ctx: &ProcessCtx, ruid: usize, euid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let old = creds.clone();
    let new_ruid = if ruid as u32 == u32::MAX {
        creds.uid
    } else {
        creds.user_ns.make_uid(ruid as u32)?
    };
    let new_euid = if euid as u32 == u32::MAX {
        creds.euid
    } else {
        creds.user_ns.make_uid(euid as u32)?
    };

    let capable = creds.caps.is_capable(CapabilitiesFlags::CAP_SETUID);

    if !capable {
        if ruid as u32 != u32::MAX && new_ruid != creds.uid && new_ruid != creds.euid {
            return Err(KernelError::NotPermitted);
        }
        if euid as u32 != u32::MAX
            && new_euid != creds.uid
            && new_euid != creds.euid
            && new_euid != creds.suid
        {
            return Err(KernelError::NotPermitted);
        }
    }

    if ruid as u32 != u32::MAX || (euid as u32 != u32::MAX && new_euid != creds.uid) {
        creds.suid = new_euid;
    }

    creds.uid = new_ruid;
    creds.euid = new_euid;

    creds.fsuid = creds.euid;
    creds.finish_id_change(&old, ctx);
    Ok(0)
}

pub fn sys_setregid(ctx: &ProcessCtx, rgid: usize, egid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let old = creds.clone();
    let new_rgid = if rgid as u32 == u32::MAX {
        creds.gid
    } else {
        creds.user_ns.make_gid(rgid as u32)?
    };
    let new_egid = if egid as u32 == u32::MAX {
        creds.egid
    } else {
        creds.user_ns.make_gid(egid as u32)?
    };

    let capable = creds.caps.is_capable(CapabilitiesFlags::CAP_SETGID);

    if !capable {
        if rgid as u32 != u32::MAX && new_rgid != creds.gid && new_rgid != creds.egid {
            return Err(KernelError::NotPermitted);
        }
        if egid as u32 != u32::MAX
            && new_egid != creds.gid
            && new_egid != creds.egid
            && new_egid != creds.sgid
        {
            return Err(KernelError::NotPermitted);
        }
    }

    if rgid as u32 != u32::MAX || (egid as u32 != u32::MAX && new_egid != creds.gid) {
        creds.sgid = new_egid;
    }

    creds.gid = new_rgid;
    creds.egid = new_egid;

    creds.fsgid = creds.egid;
    creds.finish_id_change(&old, ctx);
    Ok(0)
}

pub fn sys_setresuid(ctx: &ProcessCtx, ruid: usize, euid: usize, suid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let old = creds.clone();
    let new_ruid = if ruid as u32 == u32::MAX {
        creds.uid
    } else {
        creds.user_ns.make_uid(ruid as u32)?
    };
    let new_euid = if euid as u32 == u32::MAX {
        creds.euid
    } else {
        creds.user_ns.make_uid(euid as u32)?
    };
    let new_suid = if suid as u32 == u32::MAX {
        creds.suid
    } else {
        creds.user_ns.make_uid(suid as u32)?
    };

    let capable = creds.caps.is_capable(CapabilitiesFlags::CAP_SETUID);

    if !capable {
        if ruid as u32 != u32::MAX
            && new_ruid != creds.uid
            && new_ruid != creds.euid
            && new_ruid != creds.suid
        {
            return Err(KernelError::NotPermitted);
        }
        if euid as u32 != u32::MAX
            && new_euid != creds.uid
            && new_euid != creds.euid
            && new_euid != creds.suid
        {
            return Err(KernelError::NotPermitted);
        }
        if suid as u32 != u32::MAX
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

    creds.fsuid = creds.euid;
    creds.finish_id_change(&old, ctx);
    Ok(0)
}

pub fn sys_setresgid(ctx: &ProcessCtx, rgid: usize, egid: usize, sgid: usize) -> Result<usize> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let old = creds.clone();
    let new_rgid = if rgid as u32 == u32::MAX {
        creds.gid
    } else {
        creds.user_ns.make_gid(rgid as u32)?
    };
    let new_egid = if egid as u32 == u32::MAX {
        creds.egid
    } else {
        creds.user_ns.make_gid(egid as u32)?
    };
    let new_sgid = if sgid as u32 == u32::MAX {
        creds.sgid
    } else {
        creds.user_ns.make_gid(sgid as u32)?
    };

    let capable = creds.caps.is_capable(CapabilitiesFlags::CAP_SETGID);

    if !capable {
        if rgid as u32 != u32::MAX
            && new_rgid != creds.gid
            && new_rgid != creds.egid
            && new_rgid != creds.sgid
        {
            return Err(KernelError::NotPermitted);
        }
        if egid as u32 != u32::MAX
            && new_egid != creds.gid
            && new_egid != creds.egid
            && new_egid != creds.sgid
        {
            return Err(KernelError::NotPermitted);
        }
        if sgid as u32 != u32::MAX
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

    creds.fsgid = creds.egid;
    creds.finish_id_change(&old, ctx);
    Ok(0)
}

pub fn sys_setfsuid(ctx: &ProcessCtx, new_id: usize) -> core::result::Result<usize, Infallible> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let old = creds.clone();
    if let Ok(uid) = creds.user_ns.make_uid(new_id as u32)
        && (uid == creds.uid
            || uid == creds.euid
            || uid == creds.suid
            || uid == creds.fsuid
            || creds.caps.is_capable(CapabilitiesFlags::CAP_SETUID))
    {
        creds.fsuid = uid;
        creds.finish_id_change(&old, ctx);
    }
    Ok(old.user_ns.show_uid(old.fsuid) as usize)
}

pub fn sys_setfsgid(ctx: &ProcessCtx, new_id: usize) -> core::result::Result<usize, Infallible> {
    let mut creds = ctx.shared().creds.lock_save_irq();
    let old = creds.clone();
    if let Ok(gid) = creds.user_ns.make_gid(new_id as u32)
        && (gid == creds.gid
            || gid == creds.egid
            || gid == creds.sgid
            || gid == creds.fsgid
            || creds.caps.is_capable(CapabilitiesFlags::CAP_SETGID))
    {
        creds.fsgid = gid;
        creds.finish_id_change(&old, ctx);
    }
    Ok(old.user_ns.show_gid(old.fsgid) as usize)
}

pub async fn sys_getgroups(ctx: &ProcessCtx, size: i32, list: TUA<Gid>) -> Result<usize> {
    if size < 0 {
        return Err(KernelError::InvalidValue);
    }
    let creds = ctx.shared().creds.lock_save_irq().clone();
    let groups: Vec<_> = creds
        .groups
        .iter()
        .map(|gid| Gid::new(creds.user_ns.show_gid(*gid)))
        .collect();
    if size != 0 {
        if (size as usize) < groups.len() {
            return Err(KernelError::InvalidValue);
        }
        copy_objs_to_user(&groups, list).await?;
    }
    Ok(groups.len())
}

pub async fn sys_setgroups(ctx: &ProcessCtx, size: usize, list: TUA<Gid>) -> Result<usize> {
    if size > 65536 {
        return Err(KernelError::InvalidValue);
    }
    let creds = ctx.shared().creds.lock_save_irq().clone();
    creds.caps.check_capable(CapabilitiesFlags::CAP_SETGID)?;
    if !creds.user_ns.may_setgroups() {
        return Err(KernelError::NotPermitted);
    }
    let groups = copy_obj_array_from_user(list, size).await?;
    let groups = groups
        .into_iter()
        .map(|gid| creds.user_ns.make_gid(gid.into()))
        .collect::<Result<Vec<_>>>()?;
    ctx.shared().creds.lock_save_irq().groups = groups;
    Ok(0)
}

pub fn sys_gettid(ctx: &ProcessCtx) -> core::result::Result<usize, Infallible> {
    let tid: u32 = ctx.shared().pid.local();

    Ok(tid as _)
}

pub async fn sys_getresuid(
    ctx: &ProcessCtx,
    ruid: TUA<Uid>,
    euid: TUA<Uid>,
    suid: TUA<Uid>,
) -> Result<usize> {
    let creds = ctx.shared().creds.lock_save_irq().clone();

    copy_to_user(ruid, Uid::new(creds.user_ns.show_uid(creds.uid))).await?;
    copy_to_user(euid, Uid::new(creds.user_ns.show_uid(creds.euid))).await?;
    copy_to_user(suid, Uid::new(creds.user_ns.show_uid(creds.suid))).await?;

    Ok(0)
}

pub async fn sys_getresgid(
    ctx: &ProcessCtx,
    rgid: TUA<Gid>,
    egid: TUA<Gid>,
    sgid: TUA<Gid>,
) -> Result<usize> {
    let creds = ctx.shared().creds.lock_save_irq().clone();

    copy_to_user(rgid, Gid::new(creds.user_ns.show_gid(creds.gid))).await?;
    copy_to_user(egid, Gid::new(creds.user_ns.show_gid(creds.egid))).await?;
    copy_to_user(sgid, Gid::new(creds.user_ns.show_gid(creds.sgid))).await?;

    Ok(0)
}

pub async fn sys_getsid(ctx: &ProcessCtx, pid: i32) -> Result<usize> {
    let process = if pid == 0 {
        ctx.shared().process.clone()
    } else {
        super::pid_namespace::find_task(ctx.shared(), pid as u32)
            .ok_or(KernelError::NoProcess)?
            .process
            .clone()
    };
    Ok(process
        .sid_ref
        .lock_save_irq()
        .in_ns(&ctx.shared().pid_ns()) as usize)
}

pub async fn sys_setsid(ctx: &ProcessCtx) -> Result<usize> {
    let _pid_op = super::pid_namespace::PID_OPS.lock_save_irq();
    let process = ctx.shared().process.clone();

    let new_sid = process.tgid.value();
    let groups: Vec<_> = super::thread_group::TG_LIST
        .lock_save_irq()
        .values()
        .filter_map(|tg| tg.upgrade())
        .collect();
    if groups
        .iter()
        .any(|tg| tg.pgid.lock_save_irq().value() == new_sid)
    {
        return Err(KernelError::NotPermitted);
    }
    *process.sid.lock_save_irq() = Sid(new_sid);
    *process.pgid.lock_save_irq() = super::thread_group::Pgid(new_sid);
    *process.sid_ref.lock_save_irq() = process.pid.clone();
    *process.pgid_ref.lock_save_irq() = process.pid.clone();
    Ok(process.pid.local() as _)
}
