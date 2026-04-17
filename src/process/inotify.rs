use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use async_trait::async_trait;
use core::{
    ffi::c_char,
    future::Future,
    mem::size_of,
    pin::Pin,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{FileType, Inode, InodeId, OpenFlags, attr::AccessMode, path::Path},
    memory::address::{TUA, UA},
};

use crate::{
    drivers::timer::sleep,
    fs::{
        VFS,
        fops::FileOps,
        open_file::{FileCtx, OpenFile},
    },
    memory::uaccess::{copy_to_user, copy_to_user_slice, cstr::UserCStr},
    process::fd_table::{Fd, FdFlags},
    sched::syscall_ctx::ProcessCtx,
    sync::{Mutex, OnceLock},
};

#[expect(unused)]
pub const IN_ACCESS: u32 = 0x0000_0001;
pub const IN_MODIFY: u32 = 0x0000_0002;
pub const IN_ATTRIB: u32 = 0x0000_0004;
#[expect(unused)]
pub const IN_CLOSE_WRITE: u32 = 0x0000_0008;
#[expect(unused)]
pub const IN_CLOSE_NOWRITE: u32 = 0x0000_0010;
#[expect(unused)]
pub const IN_OPEN: u32 = 0x0000_0020;
pub const IN_MOVED_FROM: u32 = 0x0000_0040;
pub const IN_MOVED_TO: u32 = 0x0000_0080;
pub const IN_CREATE: u32 = 0x0000_0100;
pub const IN_DELETE: u32 = 0x0000_0200;
pub const IN_DELETE_SELF: u32 = 0x0000_0400;
pub const IN_MOVE_SELF: u32 = 0x0000_0800;
pub const IN_UNMOUNT: u32 = 0x0000_2000;
pub const IN_Q_OVERFLOW: u32 = 0x0000_4000;
pub const IN_IGNORED: u32 = 0x0000_8000;
pub const IN_ALL_EVENTS: u32 = 0x0000_0fff;

pub const IN_ONLYDIR: u32 = 0x0100_0000;
pub const IN_DONT_FOLLOW: u32 = 0x0200_0000;
pub const IN_EXCL_UNLINK: u32 = 0x0400_0000;
pub const IN_MASK_CREATE: u32 = 0x1000_0000;
pub const IN_MASK_ADD: u32 = 0x2000_0000;
pub const IN_ISDIR: u32 = 0x4000_0000;
pub const IN_ONESHOT: u32 = 0x8000_0000;

const INOTIFY_ALLOWED_MASK: u32 = IN_ALL_EVENTS
    | IN_ONLYDIR
    | IN_DONT_FOLLOW
    | IN_EXCL_UNLINK
    | IN_MASK_CREATE
    | IN_MASK_ADD
    | IN_ONESHOT;
const INOTIFY_STORED_MASK: u32 = IN_ALL_EVENTS | IN_EXCL_UNLINK | IN_ONESHOT;
const INOTIFY_READ_FLAGS: u32 = OpenFlags::O_NONBLOCK.bits() | OpenFlags::O_CLOEXEC.bits();

static NEXT_INOTIFY_INSTANCE_ID: AtomicUsize = AtomicUsize::new(1);
static NEXT_INOTIFY_COOKIE: AtomicU32 = AtomicU32::new(1);
static INOTIFY_REGISTRY: OnceLock<Mutex<InotifyRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<InotifyRegistry> {
    INOTIFY_REGISTRY.get_or_init(|| Mutex::new(InotifyRegistry::default()))
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct InotifyEvent {
    pub wd: i32,
    pub mask: u32,
    pub cookie: u32,
    pub len: u32,
}

unsafe impl crate::memory::uaccess::UserCopyable for InotifyEvent {}

#[derive(Clone)]
struct QueuedEvent {
    header: InotifyEvent,
    name: Vec<u8>,
}

impl QueuedEvent {
    fn new(wd: i32, mask: u32, cookie: u32, name: Option<&str>) -> Self {
        let name = name.map(encode_name).unwrap_or_default();

        Self {
            header: InotifyEvent {
                wd,
                mask,
                cookie,
                len: name.len() as u32,
            },
            name,
        }
    }

    fn total_len(&self) -> usize {
        size_of::<InotifyEvent>() + self.name.len()
    }
}

fn encode_name(name: &str) -> Vec<u8> {
    let base_len = name.len() + 1;
    let padded_len = (base_len + 3) & !3;
    let mut buf = vec![0; padded_len];
    buf[..name.len()].copy_from_slice(name.as_bytes());
    buf
}

#[derive(Clone, Copy)]
struct Watch {
    wd: i32,
    inode_id: InodeId,
    mask: u32,
}

#[derive(Default)]
struct InotifyState {
    next_wd: i32,
    watches_by_wd: BTreeMap<i32, Watch>,
    wd_by_inode: BTreeMap<InodeId, i32>,
    queue: VecDeque<QueuedEvent>,
}

impl InotifyState {
    fn alloc_wd(&mut self) -> i32 {
        let wd = self.next_wd.max(1);
        self.next_wd = wd.saturating_add(1);
        wd
    }
}

pub struct Inotify {
    inner: Arc<InotifyInner>,
}

impl Inotify {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(InotifyInner {
                id: NEXT_INOTIFY_INSTANCE_ID.fetch_add(1, Ordering::Relaxed),
                state: Mutex::new(InotifyState::default()),
            }),
        }
    }

    async fn add_watch(&mut self, inode: Arc<dyn Inode>, mask: u32) -> Result<i32> {
        self.inner.add_watch(inode, mask).await
    }

    async fn rm_watch(&mut self, wd: i32) -> Result<()> {
        self.inner.rm_watch(wd).await
    }

    async fn release_all_watches(&mut self) {
        self.inner.release_all_watches().await;
    }

    async fn read_impl(&mut self, buf: UA, count: usize, nonblock: bool) -> Result<usize> {
        if count < size_of::<InotifyEvent>() {
            return Err(KernelError::InvalidValue);
        }

        let mut dst = buf;
        let mut bytes_read = 0usize;

        loop {
            let event = {
                let mut state = self.inner.state.lock().await;
                let next_len = state.queue.front().map(|event| event.total_len());

                match next_len {
                    Some(next_len) if next_len <= count - bytes_read => state.queue.pop_front(),
                    Some(_) if bytes_read == 0 => return Err(KernelError::InvalidValue),
                    Some(_) => return Ok(bytes_read),
                    None if bytes_read > 0 => return Ok(bytes_read),
                    None if nonblock => return Err(KernelError::TryAgain),
                    None => None,
                }
            };

            let Some(event) = event else {
                sleep(core::time::Duration::from_millis(10)).await;
                continue;
            };

            copy_to_user(TUA::from_value(dst.value()), event.header).await?;
            dst = dst.add_bytes(size_of::<InotifyEvent>());
            if !event.name.is_empty() {
                copy_to_user_slice(&event.name, dst).await?;
                dst = dst.add_bytes(event.name.len());
            }

            bytes_read += event.total_len();
            if bytes_read == count {
                return Ok(bytes_read);
            }
        }
    }
}

#[async_trait]
impl FileOps for Inotify {
    async fn read(&mut self, ctx: &mut FileCtx, buf: UA, count: usize) -> Result<usize> {
        self.read_impl(buf, count, ctx.flags.contains(OpenFlags::O_NONBLOCK))
            .await
    }

    async fn readat(&mut self, buf: UA, count: usize, _offset: u64) -> Result<usize> {
        self.read_impl(buf, count, false).await
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::InvalidValue)
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + 'static + Send>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            loop {
                if !inner.state.lock().await.queue.is_empty() {
                    return Ok(());
                }
                sleep(core::time::Duration::from_millis(10)).await;
            }
        })
    }

    async fn release(&mut self, _ctx: &FileCtx) -> Result<()> {
        self.release_all_watches().await;
        Ok(())
    }

    fn as_inotify(&mut self) -> Option<&mut crate::process::inotify::Inotify> {
        Some(self)
    }
}

struct InotifyInner {
    id: usize,
    state: Mutex<InotifyState>,
}

impl InotifyInner {
    async fn add_watch(self: &Arc<Self>, inode: Arc<dyn Inode>, mask: u32) -> Result<i32> {
        let inode_id = inode.id();
        let requested = mask & INOTIFY_STORED_MASK;
        let mask_add = mask & IN_MASK_ADD != 0;
        let mask_create = mask & IN_MASK_CREATE != 0;

        let existing_wd = {
            let mut state = self.state.lock().await;

            if let Some(&wd) = state.wd_by_inode.get(&inode_id) {
                let watch = state
                    .watches_by_wd
                    .get_mut(&wd)
                    .ok_or(KernelError::InvalidValue)?;

                if mask_create {
                    return Err(FsError::AlreadyExists.into());
                }

                watch.mask = if mask_add {
                    watch.mask | requested
                } else {
                    requested
                };

                Some(wd)
            } else {
                None
            }
        };

        if let Some(wd) = existing_wd {
            return Ok(wd);
        }

        let wd = {
            let mut state = self.state.lock().await;
            let wd = state.alloc_wd();
            let watch = Watch {
                wd,
                inode_id,
                mask: requested,
            };
            state.wd_by_inode.insert(inode_id, wd);
            state.watches_by_wd.insert(wd, watch);
            wd
        };

        registry_add_watch(inode_id, self.clone(), wd).await;

        Ok(wd)
    }

    async fn rm_watch(&self, wd: i32) -> Result<()> {
        let inode_id = {
            let mut state = self.state.lock().await;
            let watch = state
                .watches_by_wd
                .remove(&wd)
                .ok_or(KernelError::InvalidValue)?;
            state.wd_by_inode.remove(&watch.inode_id);
            watch.inode_id
        };

        registry_remove_watch(inode_id, self.id, wd).await;
        self.enqueue_unconditional(wd, IN_IGNORED, 0, None).await;

        Ok(())
    }

    async fn release_all_watches(&self) {
        let removed = {
            let mut state = self.state.lock().await;
            let removed = state
                .watches_by_wd
                .values()
                .map(|watch| (watch.inode_id, watch.wd))
                .collect::<Vec<_>>();
            state.watches_by_wd.clear();
            state.wd_by_inode.clear();
            state.queue.clear();
            removed
        };

        for (inode_id, wd) in removed {
            registry_remove_watch(inode_id, self.id, wd).await;
        }
    }

    async fn enqueue_filtered(&self, wd: i32, mask: u32, cookie: u32, name: Option<&str>) {
        let should_enqueue = {
            let state = self.state.lock().await;
            let Some(watch) = state.watches_by_wd.get(&wd) else {
                return;
            };

            let always = IN_IGNORED | IN_Q_OVERFLOW | IN_UNMOUNT;
            if mask & always != 0 {
                true
            } else {
                (watch.mask & IN_ALL_EVENTS) & (mask & IN_ALL_EVENTS) != 0
            }
        };

        if should_enqueue {
            self.enqueue_unconditional(wd, mask, cookie, name).await;
        }
    }

    async fn enqueue_unconditional(&self, wd: i32, mask: u32, cookie: u32, name: Option<&str>) {
        self.state
            .lock()
            .await
            .queue
            .push_back(QueuedEvent::new(wd, mask, cookie, name));
    }
}

#[derive(Default)]
struct InotifyRegistry {
    by_inode: BTreeMap<InodeId, Vec<RegistryEntry>>,
}

#[derive(Clone)]
struct RegistryEntry {
    instance_id: usize,
    wd: i32,
    weak: Weak<InotifyInner>,
}

async fn registry_add_watch(inode_id: InodeId, inner: Arc<InotifyInner>, wd: i32) {
    let mut reg = registry().lock().await;
    let entries = reg.by_inode.entry(inode_id).or_default();

    if entries
        .iter()
        .any(|entry| entry.instance_id == inner.id && entry.wd == wd)
    {
        return;
    }

    entries.push(RegistryEntry {
        instance_id: inner.id,
        wd,
        weak: Arc::downgrade(&inner),
    });
}

async fn registry_remove_watch(inode_id: InodeId, instance_id: usize, wd: i32) {
    let mut reg = registry().lock().await;

    let should_remove = if let Some(entries) = reg.by_inode.get_mut(&inode_id) {
        entries.retain(|entry| !(entry.instance_id == instance_id && entry.wd == wd));
        entries.is_empty()
    } else {
        false
    };

    if should_remove {
        reg.by_inode.remove(&inode_id);
    }
}

async fn dispatch_event(inode_id: InodeId, mask: u32, cookie: u32, name: Option<&str>) {
    let deliveries = {
        let mut reg = registry().lock().await;
        let mut deliveries = Vec::new();

        let should_remove = if let Some(entries) = reg.by_inode.get_mut(&inode_id) {
            entries.retain(|entry| {
                if let Some(inner) = entry.weak.upgrade() {
                    deliveries.push((entry.wd, inner));
                    true
                } else {
                    false
                }
            });
            entries.is_empty()
        } else {
            false
        };

        if should_remove {
            reg.by_inode.remove(&inode_id);
        }

        deliveries
    };

    for (wd, inner) in deliveries {
        inner.enqueue_filtered(wd, mask, cookie, name).await;
    }
}

pub async fn notify_modify(inode_id: InodeId) {
    dispatch_event(inode_id, IN_MODIFY, 0, None).await;
}

pub async fn notify_attrib(inode_id: InodeId) {
    dispatch_event(inode_id, IN_ATTRIB, 0, None).await;
}

pub async fn notify_create(parent_inode_id: InodeId, name: &str, is_dir: bool) {
    let mask = IN_CREATE | if is_dir { IN_ISDIR } else { 0 };
    dispatch_event(parent_inode_id, mask, 0, Some(name)).await;
}

pub async fn notify_delete(parent_inode_id: InodeId, name: &str, is_dir: bool) {
    let mask = IN_DELETE | if is_dir { IN_ISDIR } else { 0 };
    dispatch_event(parent_inode_id, mask, 0, Some(name)).await;
}

pub async fn notify_delete_self(inode_id: InodeId, is_dir: bool) {
    let mask = IN_DELETE_SELF | if is_dir { IN_ISDIR } else { 0 };
    dispatch_event(inode_id, mask, 0, None).await;
}

pub async fn notify_move(
    old_parent_inode_id: InodeId,
    old_name: &str,
    new_parent_inode_id: InodeId,
    new_name: &str,
    target_inode_id: InodeId,
    is_dir: bool,
) {
    let cookie = NEXT_INOTIFY_COOKIE.fetch_add(1, Ordering::Relaxed);
    let dir_flag = if is_dir { IN_ISDIR } else { 0 };

    dispatch_event(
        old_parent_inode_id,
        IN_MOVED_FROM | dir_flag,
        cookie,
        Some(old_name),
    )
    .await;
    dispatch_event(
        new_parent_inode_id,
        IN_MOVED_TO | dir_flag,
        cookie,
        Some(new_name),
    )
    .await;
    dispatch_event(target_inode_id, IN_MOVE_SELF | dir_flag, cookie, None).await;
}

pub async fn sys_inotify_init1(ctx: &ProcessCtx, flags: u32) -> Result<usize> {
    if flags & !INOTIFY_READ_FLAGS != 0 {
        return Err(KernelError::InvalidValue);
    }

    let file_flags = if flags & OpenFlags::O_NONBLOCK.bits() != 0 {
        OpenFlags::O_NONBLOCK
    } else {
        OpenFlags::empty()
    };
    let fd_flags = if flags & OpenFlags::O_CLOEXEC.bits() != 0 {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };

    let file = Arc::new(OpenFile::new(Box::new(Inotify::new()), file_flags));
    let fd = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .insert_with_flags(file, fd_flags)?;

    Ok(fd.as_raw() as usize)
}

pub async fn sys_inotify_add_watch(
    ctx: &ProcessCtx,
    fd: Fd,
    pathname: TUA<c_char>,
    mask: u32,
) -> Result<usize> {
    if mask & !INOTIFY_ALLOWED_MASK != 0
        || mask & IN_ALL_EVENTS == 0
        || (mask & IN_MASK_ADD != 0 && mask & IN_MASK_CREATE != 0)
    {
        return Err(KernelError::InvalidValue);
    }

    let task = ctx.shared().clone();
    let mut buf = [0; 1024];
    let path = Path::new(
        UserCStr::from_ptr(pathname)
            .copy_from_user(&mut buf)
            .await?,
    );
    let cwd = task.cwd.lock_save_irq().0.clone();

    let inode = if mask & IN_DONT_FOLLOW != 0 {
        VFS.resolve_path_nofollow(path, cwd, &task).await?
    } else {
        VFS.resolve_path(path, cwd, &task).await?
    };

    let attr = inode.getattr().await?;

    if mask & IN_ONLYDIR != 0 && attr.file_type != FileType::Directory {
        return Err(FsError::NotADirectory.into());
    }

    {
        let creds = task.creds.lock_save_irq();
        if attr
            .check_access(creds.euid(), creds.egid(), creds.caps(), AccessMode::R_OK)
            .is_err()
        {
            return Err(FsError::PermissionDenied.into());
        }
    }

    let inotify_file = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, _) = &mut *inotify_file.lock().await;
    let inotify = ops.as_inotify().ok_or(KernelError::InvalidValue)?;

    Ok(inotify.add_watch(inode, mask).await? as usize)
}

pub async fn sys_inotify_rm_watch(ctx: &ProcessCtx, fd: Fd, wd: i32) -> Result<usize> {
    let task = ctx.shared().clone();
    let inotify_file = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, _) = &mut *inotify_file.lock().await;
    let inotify = ops.as_inotify().ok_or(KernelError::InvalidValue)?;
    inotify.rm_watch(wd).await?;

    Ok(0)
}
