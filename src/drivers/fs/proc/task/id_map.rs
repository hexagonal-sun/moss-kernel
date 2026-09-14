//! Proc ID-map controls are open-file operations: they pin the namespace and
//! the opener's credentials, including when an fd is inherited by another task.
use crate::{
    fs::{fops::FileOps, open_file::FileCtx},
    memory::uaccess::{copy_from_user_slice, copy_to_user_slice},
    process::{
        Tid,
        creds::Credentials,
        find_task_by_tid,
        user_namespace::{MapKind, UserNamespace},
    },
};
use alloc::{boxed::Box, sync::Arc};
use async_trait::async_trait;
use core::any::Any;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{
        FileType, Inode, InodeId, SeekFrom,
        attr::{FileAttr, FilePermissions},
    },
    memory::address::UA,
};

#[derive(Clone, Copy)]
pub enum Control {
    Map(MapKind),
    Setgroups,
}

impl Control {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "uid_map" => Some(Self::Map(MapKind::Uid)),
            "gid_map" => Some(Self::Map(MapKind::Gid)),
            "setgroups" => Some(Self::Setgroups),
            _ => None,
        }
    }
}

pub struct IdMapInode {
    pub tid: Tid,
    pub id: InodeId,
    pub control: Control,
}

#[async_trait]
impl Inode for IdMapInode {
    fn id(&self) -> InodeId {
        self.id
    }
    async fn truncate(&self, size: u64) -> Result<()> {
        check_truncate(size)
    }
    async fn getattr(&self) -> Result<FileAttr> {
        let task = find_task_by_tid(self.tid).ok_or(FsError::NotFound)?;
        let creds = task.creds.lock_save_irq();
        let mut attr = FileAttr {
            id: self.id,
            file_type: FileType::File,
            permissions: FilePermissions::from_bits_retain(0o644),
            uid: creds.euid(),
            gid: creds.egid(),
            ..FileAttr::default()
        };
        if task
            .process
            .dumpable
            .load(core::sync::atomic::Ordering::Acquire)
            != 1
        {
            attr.uid = creds
                .user_ns()
                .make_uid(0)
                .unwrap_or(libkernel::proc::ids::Uid::new_root());
            attr.gid = creds
                .user_ns()
                .make_gid(0)
                .unwrap_or(libkernel::proc::ids::Gid::new_root_group());
        }
        Ok(attr)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn open(inode: &dyn Inode, opener: &Credentials) -> Result<Option<Box<dyn FileOps>>> {
    let Some(inode) = inode.as_any().downcast_ref::<IdMapInode>() else {
        return Ok(None);
    };
    let task = find_task_by_tid(inode.tid).ok_or(KernelError::NoProcess)?;
    let ns = task.creds.lock_save_irq().user_ns();
    Ok(Some(Box::new(IdMapFile {
        ns,
        opener: opener.clone(),
        control: inode.control,
    })))
}

struct IdMapFile {
    ns: Arc<UserNamespace>,
    opener: Credentials,
    control: Control,
}

fn check_truncate(size: u64) -> Result<()> {
    // Like proc_setattr, truncation changes no control state. In particular,
    // shell redirection (O_TRUNC) must neither fail nor reset a write-once map.
    if size > i64::MAX as u64 {
        Err(KernelError::InvalidValue)
    } else {
        Ok(())
    }
}

#[async_trait]
impl FileOps for IdMapFile {
    async fn truncate(&mut self, _ctx: &FileCtx, size: usize) -> Result<()> {
        check_truncate(size as u64)
    }

    async fn readat(&mut self, buf: UA, count: usize, offset: u64) -> Result<usize> {
        let text = match self.control {
            Control::Map(kind) => self.ns.read_map(kind, &self.opener.user_ns()),
            Control::Setgroups => self.ns.read_setgroups().into(),
        };
        let start = (offset as usize).min(text.len());
        let end = text.len().min(start.saturating_add(count));
        copy_to_user_slice(&text.as_bytes()[start..end], buf).await?;
        Ok(end - start)
    }

    async fn writeat(&mut self, buf: UA, count: usize, offset: u64) -> Result<usize> {
        if count >= 4096 || offset != 0 {
            return Err(KernelError::InvalidValue);
        }
        let mut data = [0u8; 4096];
        copy_from_user_slice(buf, &mut data[..count]).await?;
        let caller = crate::sched::current_work()
            .task
            .creds
            .lock_save_irq()
            .clone();
        match self.control {
            Control::Map(kind) => {
                self.ns
                    .write_map(kind, &data[..count], offset, &self.opener, &caller)
            }
            Control::Setgroups => {
                self.ns
                    .write_setgroups(&data[..count], offset, &self.opener, &caller)
            }
        }
    }

    async fn seek(&mut self, ctx: &mut FileCtx, pos: SeekFrom) -> Result<u64> {
        ctx.pos = match pos {
            SeekFrom::Start(pos) if pos <= i64::MAX as u64 => pos,
            SeekFrom::Current(delta) => ctx
                .pos
                .checked_add_signed(delta)
                .filter(|pos| *pos <= i64::MAX as u64)
                .ok_or(KernelError::InvalidValue)?,
            _ => return Err(KernelError::InvalidValue),
        };
        Ok(ctx.pos)
    }
}
