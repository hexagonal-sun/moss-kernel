use alloc::{boxed::Box, sync::Arc, vec::Vec};
use async_trait::async_trait;
use core::any::Any;
use core::{future::Future, pin::Pin};
use libkernel::{
    error::{KernelError, Result},
    fs::{OpenFlags, SeekFrom},
    memory::address::{TUA, UA},
    sync::condvar::WakeupType,
};

use crate::memory::uaccess::copy_from_user;

use crate::{
    fs::{fops::FileOps, open_file::FileCtx, open_file::OpenFile},
    process::{
        Task,
        fd_table::Fd,
        thread_group::signal::{SigId, SigSet},
    },
    sched::current::current_task_shared,
    sync::{CondVar, Mutex},
};

pub const SFD_CLOEXEC: i32 = 0x0008_0000;
pub const SFD_NONBLOCK: i32 = 0x0000_8000;

/// Kernel object backing one signalfd file-descriptor.
struct SignalFd {
    mask: SigSet,
    queue: Mutex<Vec<u32>>,
    cv: CondVar<bool>,
    nonblock: bool,
}

impl SignalFd {
    fn new(mask: SigSet, nonblock: bool) -> Self {
        Self {
            mask,
            queue: Mutex::new(Vec::new()),
            cv: CondVar::new(false),
            nonblock,
        }
    }

    /// Enqueue a newly delivered signal if it matches the interest mask.
    async fn notify_signal(&self, sig: SigId) {
        if self.mask.contains(sig.into()) && sig != SigId::SIGKILL && sig != SigId::SIGSTOP {
            let mut q = self.queue.lock().await;
            q.push(sig.user_id() as u32);
            drop(q);
            self.cv.update(|flag| {
                *flag = true;
                WakeupType::All
            });
        }
    }

    async fn pop_signal(&self) -> Option<u32> {
        let mut q = self.queue.lock().await;
        if !q.is_empty() {
            let sig = q.remove(0);
            let empty = q.is_empty();
            drop(q);
            if empty {
                self.cv.update(|flag| {
                    *flag = false;
                    WakeupType::None
                });
            }
            Some(sig)
        } else {
            None
        }
    }
}

#[async_trait]
impl FileOps for SignalFd {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    async fn readat(&mut self, buf: UA, count: usize, _offset: u64) -> Result<usize> {
        // Reading anything but multiples of 4 bytes is rejected – we only
        // return a list of u32 signal numbers.
        if count < size_of::<u32>() {
            return Err(KernelError::InvalidValue);
        }

        // Attempt to get one pending signal.
        if let Some(sig) = self.pop_signal().await {
            let bytes = (sig).to_ne_bytes();
            crate::memory::uaccess::copy_to_user_slice(&bytes, buf).await?;
            Ok(size_of::<u32>())
        } else if self.nonblock {
            Err(KernelError::TryAgain)
        } else {
            // Wait until a signal arrives.
            self.cv
                .wait_until(|s| if *s { Some(()) } else { None })
                .await;
            // Recurse – now there must be something.
            self.readat(buf, count, 0).await
        }
    }

    async fn writeat(&mut self, _buf: UA, _count: usize, _offset: u64) -> Result<usize> {
        Err(KernelError::BadFd)
    }

    fn poll_read_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        let cv = self.cv.clone();
        Box::pin(async move {
            cv.wait_until(|flag| if *flag { Some(()) } else { None })
                .await;
            Ok(())
        })
    }

    fn poll_write_ready(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
        Box::pin(async { Err(KernelError::NotSupported) })
    }

    async fn seek(&mut self, _ctx: &mut FileCtx, _pos: SeekFrom) -> Result<u64> {
        Err(KernelError::InvalidValue)
    }

    async fn release(&mut self, _ctx: &FileCtx) -> Result<()> {
        Ok(())
    }
}

pub async fn sys_signalfd4(fd: Fd, mask_ptr: TUA<SigSet>, flags: i32) -> Result<usize> {
    // SIGKILL/SIGSTOP must be silently ignored – clear them from the mask.
    let mut mask = copy_from_user(mask_ptr).await?;
    mask.remove(SigId::SIGKILL.into());
    mask.remove(SigId::SIGSTOP.into());

    let nonblock = (flags & SFD_NONBLOCK) != 0;
    let cloexec = (flags & SFD_CLOEXEC) != 0;

    let task = current_task_shared();

    if fd.as_raw() == -1 {
        // Create a brand-new signalfd.
        let sfd = Arc::new(OpenFile::new(Box::new(SignalFd::new(mask, nonblock)), {
            let mut of = OpenFlags::empty();
            if nonblock {
                of |= OpenFlags::O_NONBLOCK;
            }
            if cloexec {
                of |= OpenFlags::O_CLOEXEC;
            }
            of
        }));
        let mut fdt = task.fd_table.lock_save_irq();
        let new_fd = fdt.insert(sfd)?;
        Ok(new_fd.as_raw() as _)
    } else {
        // Modify an existing one.
        let file = {
            let fdt = task.fd_table.lock_save_irq();
            fdt.get(fd).ok_or(KernelError::BadFd)?
        };

        // Verify this really is a signalfd instance.
        {
            let (ops, _) = &mut *file.lock().await;
            if let Some(sigops) = ops.as_any_mut().downcast_mut::<SignalFd>() {
                sigops.mask = mask;
                sigops.nonblock = nonblock;
            } else {
                return Err(KernelError::InvalidValue);
            }
        }

        {
            let mut new_flags = file.flags().await;
            if nonblock {
                new_flags |= OpenFlags::O_NONBLOCK;
            } else {
                new_flags.remove(OpenFlags::O_NONBLOCK);
            }
            if cloexec {
                new_flags |= OpenFlags::O_CLOEXEC;
            } else {
                new_flags.remove(OpenFlags::O_CLOEXEC);
            }
            file.set_flags(new_flags).await;
        }

        Ok(fd.as_raw() as _)
    }
}

/// Notify all signalfd instances in the current task’s FD table about a newly
/// delivered signal.
pub async fn broadcast_to_signalfds(task: Arc<Task>, signal: SigId) {
    // Collect all open files from the task's FD table (maximum 8192 fds).
    const MAX_FDS_SCAN: usize = 8192;
    let mut files = Vec::new();
    {
        let fdt_guard = task.fd_table.lock_save_irq();
        for i in 0..MAX_FDS_SCAN {
            if let Some(file) = fdt_guard.get(Fd(i as i32)) {
                files.push(file);
            }
        }
    }

    for file in files {
        let (ops, _) = &*file.lock().await;
        if let Some(sigops) = ops.as_any().downcast_ref::<SignalFd>() {
            sigops.notify_signal(signal).await;
        }
    }
}
