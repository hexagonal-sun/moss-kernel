use super::{SigId, SigSet};
use crate::fs::fops::FileOps;
use crate::fs::open_file::{FileCtx, OpenFile};
use crate::memory::uaccess::{copy_from_user, copy_to_user};
use crate::process::fd_table::{Fd, FdFlags};
use crate::sched::{current_work, sched_task::Work, syscall_ctx::ProcessCtx};
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use libkernel::error::{KernelError, Result};
use libkernel::fs::OpenFlags;
use libkernel::memory::address::{TUA, UA};

// Structure returned by read(2) on a signalfd
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SignalfdSiginfo {
    pub ssi_signo: u32,
    pub ssi_errno: i32,
    pub ssi_code: i32,
    pub ssi_pid: u32,
    pub ssi_uid: u32,
    pub ssi_fd: i32,
    pub ssi_tid: u32,
    pub ssi_band: u32,
    pub ssi_overrun: u32,
    pub ssi_trapno: u32,
    pub ssi_status: i32,
    pub ssi_int: i32,
    pub ssi_ptr: u64,
    pub ssi_utime: u64,
    pub ssi_stime: u64,
    pub ssi_addr: u64,
    pub pad: [u8; 48],
}

impl Default for SignalfdSiginfo {
    fn default() -> Self {
        Self {
            ssi_signo: 0,
            ssi_errno: 0,
            ssi_code: 0,
            ssi_pid: 0,
            ssi_uid: 0,
            ssi_fd: 0,
            ssi_tid: 0,
            ssi_band: 0,
            ssi_overrun: 0,
            ssi_trapno: 0,
            ssi_status: 0,
            ssi_int: 0,
            ssi_ptr: 0,
            ssi_utime: 0,
            ssi_stime: 0,
            ssi_addr: 0,
            pad: [0; 48],
        }
    }
}

unsafe impl crate::memory::uaccess::UserCopyable for SignalfdSiginfo {}

pub struct SignalFd {
    mask: SigSet,
}

impl SignalFd {
    pub fn new(mask: SigSet) -> Self {
        Self {
            mask: sanitize_mask(mask),
        }
    }

    pub fn set_mask(&mut self, mask: SigSet) {
        self.mask = sanitize_mask(mask);
    }

    fn blocked_mask(&self) -> SigSet {
        SigSet::from_bits_truncate(!self.mask.bits())
    }

    fn take_pending_signal_for(task: &Work, blocked: SigSet) -> Option<SigId> {
        task.pending_signals.take_signal(blocked).or_else(|| {
            task.process
                .pending_signals
                .lock_save_irq()
                .take_signal(blocked)
        })
    }

    fn take_pending_signal(&self) -> Option<SigId> {
        Self::take_pending_signal_for(&current_work(), self.blocked_mask())
    }

    fn has_pending_signal_for(task: &Work, blocked: SigSet) -> bool {
        task.pending_signals.peek_signal(blocked).is_some()
            || task
                .process
                .pending_signals
                .lock_save_irq()
                .peek_signal(blocked)
                .is_some()
    }

    async fn wait_for_pending_signal(&self) {
        SignalFdWait::new(current_work(), self.blocked_mask()).await;
    }

    async fn read_impl(&mut self, buf: UA, count: usize, nonblock: bool) -> Result<usize> {
        let siginfo_size = core::mem::size_of::<SignalfdSiginfo>();
        if count < siginfo_size {
            return Err(KernelError::InvalidValue);
        }

        let mut bytes_read = 0;
        let mut ptr = buf;

        loop {
            if let Some(sig) = self.take_pending_signal() {
                let info = SignalfdSiginfo {
                    ssi_signo: sig.user_id() as u32,
                    ..Default::default()
                };

                let sig_tua = ptr.cast();
                copy_to_user(sig_tua, info).await?;

                ptr = ptr.add_bytes(siginfo_size);
                bytes_read += siginfo_size;

                if bytes_read + siginfo_size > count {
                    break;
                }
            } else if bytes_read > 0 {
                break;
            } else if nonblock {
                return Err(KernelError::TryAgain);
            } else {
                self.wait_for_pending_signal().await;
            }
        }

        Ok(bytes_read)
    }
}

fn sanitize_mask(mut mask: SigSet) -> SigSet {
    mask.remove(SigSet::UNMASKABLE_SIGNALS);
    mask
}

struct SignalFdWait {
    task: Arc<Work>,
    blocked: SigSet,
    token: Option<u64>,
}

impl SignalFdWait {
    fn new(task: Arc<Work>, blocked: SigSet) -> Self {
        Self {
            task,
            blocked,
            token: None,
        }
    }
}

impl Drop for SignalFdWait {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.task.signal_notifier.lock_save_irq().remove(token);
        }
    }
}

impl Future for SignalFdWait {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.as_mut().get_unchecked_mut() };
        let mut notifier = this.task.signal_notifier.lock_save_irq();

        if SignalFd::has_pending_signal_for(&this.task, this.blocked) {
            if let Some(token) = this.token.take() {
                notifier.remove(token);
            }
            return Poll::Ready(());
        }

        if let Some(token) = this.token.take() {
            notifier.remove(token);
        }
        this.token = Some(notifier.register(cx.waker()));

        Poll::Pending
    }
}

#[async_trait::async_trait]
impl FileOps for SignalFd {
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
        let mask = self.mask;
        Box::pin(async move {
            let signalfd = SignalFd { mask };
            signalfd.wait_for_pending_signal().await;
            Ok(())
        })
    }

    fn as_signalfd(&mut self) -> Option<&mut SignalFd> {
        Some(self)
    }
}

pub async fn sys_signalfd4(
    ctx: &ProcessCtx,
    fd: i32,
    user_mask: TUA<SigSet>,
    sizemask: usize,
    flags: i32,
) -> Result<usize> {
    let allowed_flags = (OpenFlags::O_NONBLOCK | OpenFlags::O_CLOEXEC).bits() as i32;
    if flags & !allowed_flags != 0 {
        return Err(KernelError::InvalidValue);
    }

    if sizemask < size_of::<SigSet>() {
        return Err(KernelError::InvalidValue);
    }

    let mask = if sizemask == size_of::<SigSet>() {
        copy_from_user(user_mask).await?
    } else {
        let val: u64 = copy_from_user(TUA::from_value(user_mask.value())).await?;
        SigSet::from_bits_truncate(val)
    };
    let mask = sanitize_mask(mask);

    let file_flags = if flags & OpenFlags::O_NONBLOCK.bits() as i32 != 0 {
        OpenFlags::O_NONBLOCK
    } else {
        OpenFlags::empty()
    };
    let fd_flags = if flags & OpenFlags::O_CLOEXEC.bits() as i32 != 0 {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };

    if fd == -1 {
        let signalfd = SignalFd::new(mask);
        let file = Arc::new(OpenFile::new(Box::new(signalfd), file_flags));
        let fd = ctx
            .shared()
            .fd_table
            .lock_save_irq()
            .insert_with_flags(file, fd_flags)?;
        Ok(fd.0 as usize)
    } else {
        let fd = Fd(fd);
        let file_arc = ctx
            .shared()
            .fd_table
            .lock_save_irq()
            .get(fd)
            .ok_or(KernelError::BadFd)?;

        {
            let (ops, file_ctx) = &mut *file_arc.lock().await;
            let signalfd = ops.as_signalfd().ok_or(KernelError::InvalidValue)?;
            signalfd.set_mask(mask);
            if file_flags.contains(OpenFlags::O_NONBLOCK) {
                file_ctx.flags.insert(OpenFlags::O_NONBLOCK);
            }
        }

        if !fd_flags.is_empty() {
            ctx.shared()
                .fd_table
                .lock_save_irq()
                .add_flags(fd, fd_flags)?;
        }

        Ok(fd.0 as usize)
    }
}
