use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::{future::Future, pin::Pin, task::Poll, time::Duration};
use core::any::Any;
use crate::{
    drivers::timer::sleep,
    fs::{
        fops::FileOps,
        open_file::{FileCtx, OpenFile}
    },
    memory::uaccess::{UserCopyable, copy_from_user, copy_objs_to_user},
    process::fd_table::{Fd, select::PollFlags},
    sched::current::current_task_shared,
    sync::Mutex,
};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::OpenFlags,
    memory::address::{TUA, UA},
};
use async_trait::async_trait;

pub const EPOLL_CTL_ADD: i32 = 1;
pub const EPOLL_CTL_DEL: i32 = 2;
pub const EPOLL_CTL_MOD: i32 = 3;

pub const EPOLLIN: u32 = 0x001;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

unsafe impl UserCopyable for EpollEvent {}

/// Entry inside an [`EpollInstance`].
#[derive(Clone)]
struct EpItem {
    file: Arc<OpenFile>,
    flags: PollFlags,
    data: u64,
}

/// The kernel object backing an epoll file-descriptor.
#[derive(Default)]
pub struct EpollInstance {
    /// Registered interest list (keyed by raw file-descriptor number).
    inner: Mutex<BTreeMap<i32, EpItem>>,
}

impl EpollInstance {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(BTreeMap::new()),
        })
    }

    /// Convert an epoll event bitmask into the internal [`PollFlags`].
    fn ep_to_poll(mask: u32) -> PollFlags {
        let mut pf = PollFlags::empty();
        if mask & EPOLLIN != 0 {
            pf.insert(PollFlags::POLLIN);
        }
        if mask & EPOLLOUT != 0 {
            pf.insert(PollFlags::POLLOUT);
        }
        if mask & EPOLLERR != 0 {
            pf.insert(PollFlags::POLLERR);
        }
        if mask & EPOLLHUP != 0 {
            pf.insert(PollFlags::POLLHUP);
        }
        pf
    }

    fn poll_to_ep(pf: PollFlags) -> u32 {
        let mut ev = 0;
        if pf.contains(PollFlags::POLLIN) {
            ev |= EPOLLIN;
        }
        if pf.contains(PollFlags::POLLOUT) {
            ev |= EPOLLOUT;
        }
        if pf.contains(PollFlags::POLLERR) {
            ev |= EPOLLERR;
        }
        if pf.contains(PollFlags::POLLHUP) {
            ev |= EPOLLHUP;
        }
        ev
    }
}

pub struct EpollFileOps {
    epi: Arc<EpollInstance>,
}

impl EpollFileOps {
    fn new(epi: Arc<EpollInstance>) -> Self {
        Self { epi }
    }
}

#[async_trait]
impl FileOps for EpollFileOps {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    async fn readat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        Box::pin(async { Err(KernelError::NotSupported) })
    }

    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        Box::pin(async { Err(KernelError::NotSupported) })
    }

    async fn release(&mut self, _ctx: &FileCtx) -> Result<()> {
        Ok(())
    }

    fn to_epoll(&self) -> Option<&EpollFileOps> {
        Some(self)
    }
}

pub fn sys_epoll_create1(flags: i32) -> Result<usize> {
    const EPOLL_CLOEXEC: i32 = 0x80000;

    let cloexec = (flags & EPOLL_CLOEXEC) != 0;
    if flags & !EPOLL_CLOEXEC != 0 {
        return Err(KernelError::InvalidValue);
    }

    // Allocate kernel object
    let epi = EpollInstance::new();

    // Wrap inside an OpenFile
    let mut oflags = OpenFlags::empty();
    if cloexec {
        oflags |= OpenFlags::O_CLOEXEC;
    }
    let file = OpenFile::new(Box::new(EpollFileOps::new(epi)), oflags);

    // Insert into current task's FD-table
    let fd = {
        let task = current_task_shared();
        let mut fdt = task.fd_table.lock_save_irq();
        fdt.insert(Arc::new(file))?
    };

    Ok(fd.as_raw() as _)
}

pub async fn sys_epoll_ctl(epfd: Fd, op: i32, fd: Fd, event: TUA<EpollEvent>) -> Result<usize> {
    // Retrieve the epoll instance.
    let epi = get_instance(epfd).await?;

    match op {
        EPOLL_CTL_ADD => {
            let ev: EpollEvent = copy_from_user(event).await?;
            let task = current_task_shared();
            let target_file = task
                .fd_table
                .lock_save_irq()
                .get(fd)
                .ok_or(KernelError::BadFd)?;

            let mut map = epi.inner.lock().await;
            if map.contains_key(&fd.as_raw()) {
                return Err(FsError::AlreadyExists)?;
            }
            map.insert(
                fd.as_raw(),
                EpItem {
                    file: target_file,
                    flags: EpollInstance::ep_to_poll(ev.events),
                    data: ev.data,
                },
            );
        }
        EPOLL_CTL_MOD => {
            let ev: EpollEvent = copy_from_user(event).await?;
            let mut map = epi.inner.lock().await;
            let entry = map.get_mut(&fd.as_raw()).ok_or(FsError::NotFound)?;

            entry.flags = EpollInstance::ep_to_poll(ev.events);
            entry.data = ev.data;
        }
        EPOLL_CTL_DEL => {
            let mut map = epi.inner.lock().await;
            if map.remove(&fd.as_raw()).is_none() {
                return Err(FsError::NotFound)?;
            }
        }
        _ => return Err(KernelError::InvalidValue),
    }

    Ok(0)
}

pub async fn sys_epoll_pwait(
    epfd: Fd,
    events: TUA<EpollEvent>,
    maxevents: i32,
    timeout_ms: i32,
    _sigmask: TUA<()>,
    _sigsetsize: usize,
) -> Result<usize> {
    if maxevents <= 0 {
        return Err(KernelError::InvalidValue);
    }

    let epi = get_instance(epfd).await?;

    // Snapshot of current interest list.
    let items: Vec<EpItem> = {
        let map = epi.inner.lock().await;
        map.values().cloned().collect()
    };

    // Build poll futures
    let mut futs: Vec<_> = Vec::<Pin<Box<dyn Future<Output = Result<PollFlags>> + Send>>>::new();
    for item in &items {
        let poll_fut = item.file.poll(item.flags).await;
        futs.push(
            Box::pin(async move { poll_fut.await }) as Pin<Box<dyn Future<Output = _> + Send>>
        );
    }

    // Optional absolute timeout
    let mut timeout_fut = if timeout_ms < 0 {
        None
    } else {
        Some(Box::pin(sleep(Duration::from_millis(timeout_ms as _)))
            as Pin<Box<dyn Future<Output = ()> + Send>>)
    };

    // Await for readiness
    let ready_count = core::future::poll_fn(|cx| {
        // Check main fd list
        let mut num_ready = 0;

        for fut in futs.iter_mut() {
            match fut.as_mut().poll(cx) {
                Poll::Ready(Ok(pf)) => {
                    if !pf.is_empty() {
                        num_ready += 1;
                    }
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => continue,
            }
        }

        if num_ready != 0 {
            Poll::Ready(Ok(num_ready))
        } else {
            // Check timeout
            if let Some(to) = timeout_fut.as_mut() {
                if to.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Ok(0));
                }
            }

            // No ready events yet, continue waiting.
            Poll::Pending
        }
    })
    .await?;

    // Copy up to `maxevents` entries to userspace
    let mut user_events: Vec<EpollEvent> = Vec::new();
    for (item, fut) in items.into_iter().zip(futs) {
        if ready_count == 0 || user_events.len() as i32 == maxevents {
            break;
        }
        // We already know it is ready (ready_count > 0) – no need to re-await.
        let flags = fut.await.unwrap_or(PollFlags::empty());
        if !flags.is_empty() {
            user_events.push(EpollEvent {
                events: EpollInstance::poll_to_ep(flags),
                data: item.data,
            });
        }
    }

    copy_objs_to_user(&user_events[..], events).await?;
    Ok(user_events.len())
}

async fn get_instance(epfd: Fd) -> Result<Arc<EpollInstance>> {
    let task = current_task_shared();
    let file = task
        .fd_table
        .lock_save_irq()
        .get(epfd)
        .ok_or(KernelError::BadFd)?;

    let (ops, _) = &*file.lock().await;

    if let Some(ep_ops) = ops.to_epoll() {
        Ok(ep_ops.epi.clone())
    } else {
        Err(KernelError::BadFd)
    }
}
