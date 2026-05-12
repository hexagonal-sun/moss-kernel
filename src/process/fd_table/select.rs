use alloc::{boxed::Box, vec::Vec};
use core::{future::poll_fn, iter, pin::pin, task::Poll};
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

use super::Fd;
use crate::{
    clock::timespec::TimeSpec,
    drivers::timer::sleep,
    memory::uaccess::{
        UserCopyable, copy_from_user, copy_obj_array_from_user, copy_objs_to_user, copy_to_user,
    },
    process::thread_group::signal::SigSet,
    sched::syscall_ctx::ProcessCtx,
};

const SET_SIZE: usize = 1024;

#[derive(Clone, Copy, Debug)]
pub struct FdSet {
    set: [u64; SET_SIZE / (8 * core::mem::size_of::<u64>())],
}

impl FdSet {
    fn iter_fds(&self, max: usize) -> impl Iterator<Item = Fd> {
        let mut candidate_fd = 0usize;

        iter::from_fn(move || {
            loop {
                if candidate_fd == max {
                    return None;
                }

                if self.set[candidate_fd / 64] & (1 << (candidate_fd % 64)) != 0 {
                    let ret = Fd(candidate_fd as i32);
                    candidate_fd += 1;

                    return Some(ret);
                }

                candidate_fd += 1;
            }
        })
    }

    fn zero(&mut self) {
        self.set = [0; _];
    }

    fn set_fd(&mut self, fd: Fd) {
        let fd = fd.as_raw();

        self.set[fd as usize / 64] |= 1 << (fd % 64);
    }
}

unsafe impl UserCopyable for FdSet {}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct PSelect6SigMask {
    sigmask: TUA<SigSet>,
    sigsetsize: usize,
}

unsafe impl UserCopyable for PSelect6SigMask {}

// TODO: handle exceptfds readiness semantics.
pub async fn sys_pselect6(
    ctx: &ProcessCtx,
    max: i32,
    readfds: TUA<FdSet>,
    writefds: TUA<FdSet>,
    exceptfds: TUA<FdSet>,
    timeout: TUA<TimeSpec>,
    mask: TUA<PSelect6SigMask>,
) -> Result<usize> {
    if max < 0 || max as usize > SET_SIZE {
        return Err(KernelError::InvalidValue);
    }

    let max = max as usize;
    let task = ctx.shared();

    let mut read_fd_set = if readfds.is_null() {
        None
    } else {
        Some(copy_from_user(readfds).await?)
    };

    let mut read_fds = Vec::new();

    let mut write_fd_set = if writefds.is_null() {
        None
    } else {
        Some(copy_from_user(writefds).await?)
    };

    let mut write_fds = Vec::new();

    let mut except_fd_set = if exceptfds.is_null() {
        None
    } else {
        Some(copy_from_user(exceptfds).await?)
    };

    let mut timeout_fut = if timeout.is_null() {
        None
    } else {
        let duration = copy_from_user(timeout).await?.into();
        Some(pin!(sleep(duration)))
    };

    if let Some(ref read_fd_set) = read_fd_set {
        for fd in read_fd_set.iter_fds(max) {
            let file = task
                .fd_table
                .lock_save_irq()
                .get(fd)
                .ok_or(KernelError::BadFd)?;

            read_fds.push((
                Box::pin(async move {
                    let (ops, _) = &mut *file.lock().await;

                    ops.poll_read_ready().await
                }),
                fd,
            ));
        }
    }

    if let Some(ref write_fd_set) = write_fd_set {
        for fd in write_fd_set.iter_fds(max) {
            let file = task
                .fd_table
                .lock_save_irq()
                .get(fd)
                .ok_or(KernelError::BadFd)?;

            write_fds.push((
                Box::pin(async move {
                    let (ops, _) = &mut *file.lock().await;

                    ops.poll_write_ready().await
                }),
                fd,
            ));
        }
    }

    let mask = if mask.is_null() {
        None
    } else {
        let args: PSelect6SigMask = copy_from_user(mask).await?;

        if args.sigsetsize != core::mem::size_of::<SigSet>() {
            return Err(KernelError::InvalidValue);
        }

        if args.sigmask.is_null() {
            None
        } else {
            Some(copy_from_user(args.sigmask).await?)
        }
    };
    let old_sigmask = task.sig_mask.load();
    if let Some(mask) = mask {
        let mut new_sigmask = mask;
        new_sigmask.remove(SigSet::UNMASKABLE_SIGNALS);
        task.sig_mask.store(new_sigmask);
    }

    if let Some(ref mut read_fd_set) = read_fd_set {
        read_fd_set.zero();
    }
    if let Some(ref mut write_fd_set) = write_fd_set {
        write_fd_set.zero();
    }
    if let Some(ref mut except_fd_set) = except_fd_set {
        except_fd_set.zero();
    }

    let n = poll_fn(|cx| {
        let mut num_ready: usize = 0;

        for (fut, fd) in read_fds.iter_mut() {
            if fut.as_mut().poll(cx).is_ready() {
                // Mark the is_ready bool. Don't break out of the loop just
                // yet, we may as well check all fds while we're here.
                if let Some(ref mut read_fd_set) = read_fd_set {
                    read_fd_set.set_fd(*fd);
                }
                num_ready += 1;
            }
        }

        for (fut, fd) in write_fds.iter_mut() {
            if fut.as_mut().poll(cx).is_ready() {
                if let Some(ref mut write_fd_set) = write_fd_set {
                    write_fd_set.set_fd(*fd);
                }
                num_ready += 1;
            }
        }

        if num_ready == 0 {
            // Check if done
            if let Some(ref mut timeout) = timeout_fut {
                timeout.as_mut().poll(cx).map(|_| 0)
            } else {
                Poll::Pending
            }
        } else {
            Poll::Ready(num_ready)
        }
    })
    .await;

    let readfds_copy_result = if let Some(read_fd_set) = read_fd_set {
        copy_to_user(readfds, read_fd_set).await
    } else {
        Ok(())
    };
    let writefds_copy_result = if let Some(write_fd_set) = write_fd_set {
        copy_to_user(writefds, write_fd_set).await
    } else {
        Ok(())
    };
    let exceptfds_copy_result = if let Some(except_fd_set) = except_fd_set {
        copy_to_user(exceptfds, except_fd_set).await
    } else {
        Ok(())
    };

    if mask.is_some() {
        task.sig_mask.store(old_sigmask);
    }

    readfds_copy_result?;
    writefds_copy_result?;
    exceptfds_copy_result?;

    Ok(n)
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct PollFlags: i16 {
        const POLLIN     = 0x001; // Read
        const POLLPRI    = 0x002; // Priority ready (mainly sockets)
        const POLLOUT    = 0x004; // Write
        const POLLERR    = 0x008; // Any errors.
        const POLLHUP    = 0x010; // Hangup
        const POLLNVAL   = 0x020;
        const POLLRDNORM = 0x040;
        const POLLRDBAND = 0x080;
        const POLLWRNORM = 0x100;
        const POLLWRBAND = 0x200;
        const POLLMSG    = 0x400;
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PollFd {
    fd: Fd,
    events: PollFlags,
    revents: PollFlags,
}

unsafe impl UserCopyable for PollFd {}

pub async fn sys_ppoll(
    ctx: &ProcessCtx,
    ufds: TUA<PollFd>,
    nfds: u32,
    timeout: TUA<TimeSpec>,
    _sigmask: TUA<SigSet>,
    _sigset_len: usize,
) -> Result<usize> {
    let task = ctx.shared();

    let mut poll_fds = copy_obj_array_from_user(ufds, nfds as _).await?;

    let mut timeout_fut = if timeout.is_null() {
        None
    } else {
        let duration = copy_from_user(timeout).await?.into();
        Some(pin!(sleep(duration)))
    };

    let fds = {
        let fd_table = task.fd_table.lock_save_irq();

        poll_fds
            .iter()
            .map(|poll_fd| fd_table.get(poll_fd.fd).ok_or(KernelError::BadFd))
            .collect::<Result<Vec<_>>>()?
    };

    let mut futs = Vec::new();

    for (poll_fd, open_file) in poll_fds.iter_mut().zip(fds) {
        let poll_fut = open_file.poll(poll_fd.events).await;

        futs.push(Box::pin(async {
            poll_fd.revents = poll_fut.await?;

            Ok(())
        }));
    }

    let num_ready = poll_fn(|cx| {
        let mut num_ready = 0;

        for fut in futs.iter_mut() {
            match fut.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => num_ready += 1,
                Poll::Ready(Err(e)) => return Poll::Ready(Err::<_, KernelError>(e)),
                Poll::Pending => continue,
            }
        }

        if num_ready == 0 {
            if let Some(ref mut timeout) = timeout_fut {
                timeout.as_mut().poll(cx).map(|_| Ok(0))
            } else {
                Poll::Pending
            }
        } else {
            Poll::Ready(Ok(num_ready))
        }
    })
    .await?;

    drop(futs);

    copy_objs_to_user(&poll_fds, ufds).await?;

    Ok(num_ready)
}
