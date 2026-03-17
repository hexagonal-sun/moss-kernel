use core::convert::Infallible;

use crate::sched::syscall_ctx::ProcessCtx;

pub fn sys_umask(ctx: &ProcessCtx, new_umask: u32) -> core::result::Result<usize, Infallible> {
    let task = ctx.shared();
    let mut umask_guard = task.process.umask.lock_save_irq();

    let old_umask = *umask_guard;

    *umask_guard = new_umask & 0o777;

    Ok(old_umask as _)
}
