use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::{future::poll_fn, pin::pin, task::Poll};
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::OpenFlags,
    memory::address::TUA,
};

use crate::{
    drivers::timer::sleep,
    fs::{fops::FileOps, open_file::OpenFile},
    memory::uaccess::{UserCopyable, copy_from_user, copy_objs_to_user},
    process::{fd_table::Fd, fd_table::select::PollFlags, thread_group::signal::SigSet},
    sched::syscall_ctx::ProcessCtx,
    sync::Mutex,
};

pub const EPOLL_CTL_ADD: i32 = 1;
pub const EPOLL_CTL_DEL: i32 = 2;
pub const EPOLL_CTL_MOD: i32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

unsafe impl UserCopyable for EpollEvent {}

pub trait EpollOps: Send + Sync {
    fn get_epoll(&mut self) -> &mut Epoll;
}

pub struct Epoll {
    watches: Mutex<BTreeMap<Fd, (EpollEvent, Arc<OpenFile>)>>,
}

impl Default for Epoll {
    fn default() -> Self {
        Self::new()
    }
}

impl Epoll {
    pub fn new() -> Self {
        Self {
            watches: Mutex::new(BTreeMap::new()),
        }
    }
}

impl EpollOps for Epoll {
    fn get_epoll(&mut self) -> &mut Epoll {
        self
    }
}

#[async_trait::async_trait]
impl FileOps for Epoll {
    async fn readat(
        &mut self,
        _buf: libkernel::memory::address::UA,
        _count: usize,
        _offset: u64,
    ) -> Result<usize> {
        Err(KernelError::NotSupported)
    }

    async fn writeat(
        &mut self,
        _buf: libkernel::memory::address::UA,
        _count: usize,
        _offset: u64,
    ) -> Result<usize> {
        Err(KernelError::NotSupported)
    }

    fn as_epoll(&mut self) -> Option<&mut dyn EpollOps> {
        Some(self)
    }
}

pub async fn sys_epoll_create1(ctx: &ProcessCtx, flags: u32) -> Result<usize> {
    if flags & !0x80000 /* EPOLL_CLOEXEC */ != 0 {
        return Err(KernelError::InvalidValue);
    }

    let task = ctx.shared();
    let epoll = Box::new(Epoll::new());
    let file = Arc::new(OpenFile::new(epoll, OpenFlags::empty()));
    let fd = task.fd_table.lock_save_irq().insert(file)?;

    // We don't implement CLOEXEC yet, but returning success is enough.

    Ok(fd.as_raw() as usize)
}

pub async fn sys_epoll_ctl(
    ctx: &ProcessCtx,
    epfd: Fd,
    op: i32,
    fd: Fd,
    event_ptr: TUA<EpollEvent>,
) -> Result<usize> {
    let task = ctx.shared();

    let epoll_file = task
        .fd_table
        .lock_save_irq()
        .get(epfd)
        .ok_or(KernelError::BadFd)?;

    let target_file = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let mut lock = epoll_file.lock().await;
    let ops = &mut lock.0;

    let epoll = ops.as_epoll().ok_or(KernelError::InvalidValue)?.get_epoll();

    let mut watches = epoll.watches.lock().await;

    match op {
        EPOLL_CTL_ADD => {
            if watches.contains_key(&fd) {
                return Err(FsError::AlreadyExists.into());
            }
            let event = copy_from_user(event_ptr).await?;
            watches.insert(fd, (event, target_file.clone()));
        }
        EPOLL_CTL_MOD => {
            if !watches.contains_key(&fd) {
                return Err(FsError::NotFound.into());
            }
            let event = copy_from_user(event_ptr).await?;
            watches.insert(fd, (event, target_file.clone()));
        }
        EPOLL_CTL_DEL => {
            if watches.remove(&fd).is_none() {
                return Err(FsError::NotFound.into());
            }
        }
        _ => return Err(KernelError::InvalidValue),
    }

    Ok(0)
}

pub async fn sys_epoll_pwait(
    ctx: &ProcessCtx,
    epfd: Fd,
    events_ptr: TUA<EpollEvent>,
    maxevents: i32,
    timeout: i32,
    sigmask: TUA<SigSet>,
    _sigsetsize: usize,
) -> Result<usize> {
    if maxevents <= 0 {
        return Err(KernelError::InvalidValue);
    }

    let task = ctx.shared();

    let epoll_file = task
        .fd_table
        .lock_save_irq()
        .get(epfd)
        .ok_or(KernelError::BadFd)?;

    let mask = if sigmask.is_null() {
        None
    } else {
        Some(copy_from_user(sigmask).await?)
    };

    let old_sigmask = task.sig_mask.load();
    if let Some(mut m) = mask {
        m.remove(SigSet::UNMASKABLE_SIGNALS);
        task.sig_mask.store(m);
    }

    let mut timeout_fut = if timeout >= 0 {
        Some(pin!(sleep(core::time::Duration::from_millis(
            timeout as u64
        ))))
    } else {
        None
    };

    // We take a snapshot of the watches.
    // we map over them and create polling futures.

    // TODO: Real implementation should perhaps be more efficient, but this works for now.
    let mut fds_and_events = {
        let mut lock = epoll_file.lock().await;
        let ops = &mut lock.0;
        let epoll = ops.as_epoll().ok_or(KernelError::InvalidValue)?.get_epoll();
        let watches = epoll.watches.lock().await;

        let mut list = Vec::new();
        for (watch_fd, (event, file)) in watches.iter() {
            let poll_flags = PollFlags::from_bits_truncate(event.events as _);
            list.push((*watch_fd, *event, file.clone(), poll_flags));
        }
        list
    };

    loop {
        // Evaluate all poll_futs for current events.
        let mut ready_events: Vec<EpollEvent> = Vec::new();
        let mut fds_to_poll = Vec::new();

        for (watch_fd, event, file, poll_flags) in fds_and_events.iter_mut() {
            fds_to_poll.push((*watch_fd, *event, file.clone(), *poll_flags));
        }

        // Wait a bit, or complete if timeout expires
        let mut events_ready = false;
        let yielded = poll_fn(|cx| {
            let mut ready = false;
            for (_watch_fd, event, file, poll_flags) in fds_to_poll.iter_mut() {
                let poll_fut = file.poll(*poll_flags);
                let mut pin_fut = Box::pin(poll_fut);
                // First poll: get the inner future
                if let Poll::Ready(inner_fut) = pin_fut.as_mut().poll(cx) {
                    let mut inner_pin = Box::pin(inner_fut);
                    // Second poll: actually poll the file readiness future with the context
                    if let Poll::Ready(Ok(revents)) = inner_pin.as_mut().poll(cx) {
                        if revents.intersects(*poll_flags) || !revents.is_empty() {
                            let mut out_event = *event;
                            out_event.events = revents.bits() as u32;
                            ready_events.push(out_event);
                            ready = true;
                            if ready_events.len() == maxevents as usize {
                                break;
                            }
                        }
                    }
                }
            }
            if ready {
                events_ready = true;
                return Poll::Ready(true);
            }

            // TODO: handle timeouts better
            if timeout == 0 {
                return Poll::Ready(true);
            }

            if let Some(ref mut t) = timeout_fut {
                if t.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(true);
                }
            }

            Poll::Pending
        })
        .await;

        if events_ready && !ready_events.is_empty() {
            copy_objs_to_user(&ready_events, events_ptr).await?;
            if mask.is_some() {
                task.sig_mask.store(old_sigmask);
            }
            return Ok(ready_events.len());
        }

        if timeout == 0 || (timeout > 0 && yielded) {
            if mask.is_some() {
                task.sig_mask.store(old_sigmask);
            }
            return Ok(0);
        }
    }
}
