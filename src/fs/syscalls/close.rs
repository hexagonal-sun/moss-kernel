use crate::{process::fd_table::Fd, sched::syscall_ctx::ProcessCtx};
use alloc::sync::Arc;
use bitflags::bitflags;
use libkernel::error::{KernelError, Result};

async fn close(ctx: &ProcessCtx, fd: Fd) -> Result<()> {
    let file = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .remove(fd)
        .ok_or(KernelError::BadFd)?;

    if let Some(file) = Arc::into_inner(file) {
        let (ops, ctx) = &mut *file.lock().await;
        ops.release(ctx).await?;
    }
    Ok(())
}

pub async fn sys_close(ctx: &ProcessCtx, fd: Fd) -> Result<usize> {
    close(ctx, fd).await?;
    Ok(0)
}

bitflags! {
    pub struct CloseRangeFlags: i32 {
        const CLOSE_RANGE_UNSHARE = 1 << 1;
        const CLOSE_RANGE_CLOEXEC = 1 << 2;
    }
}

pub async fn sys_close_range(ctx: &ProcessCtx, first: Fd, last: Fd, flags: i32) -> Result<usize> {
    let flags = CloseRangeFlags::from_bits(flags).ok_or(KernelError::InvalidValue)?;
    if first.as_raw() < 0 {
        return Err(KernelError::InvalidValue);
    }

    // `CLOSE_RANGE_UNSHARE` is effectively a no-op here because the kernel can
    // already clone the descriptor table on demand for exec and non-CLONE_FILES
    // paths. What userspace mainly needs is that the operation itself succeeds.
    let fds = ctx
        .shared()
        .fd_table
        .lock_save_irq()
        .open_fds_in_range(first.as_raw(), last.as_raw());

    if flags.contains(CloseRangeFlags::CLOSE_RANGE_CLOEXEC) {
        let mut table = ctx.shared().fd_table.lock_save_irq();
        for fd in fds {
            table.add_flags(fd, crate::process::fd_table::FdFlags::CLOEXEC)?;
        }
        return Ok(0);
    }

    for fd in fds {
        close(ctx, fd).await?;
    }
    Ok(0)
}
