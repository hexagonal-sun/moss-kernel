use core::ffi::c_long;
use core::mem::size_of;

use crate::sched::syscall_ctx::ProcessCtx;
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

pub mod futex;

pub fn sys_set_tid_address(ctx: &mut ProcessCtx, tidptr: TUA<u32>) -> Result<usize> {
    let task = ctx.task_mut();

    task.child_tid_ptr = Some(tidptr);

    Ok(task.tid.value() as _)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RobustList {
    next: TUA<RobustList>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RobustListHead {
    list: RobustList,
    futex_offset: c_long,
    list_op_pending: RobustList,
}

pub async fn sys_set_robust_list(
    ctx: &mut ProcessCtx,
    head: TUA<RobustListHead>,
    len: usize,
) -> Result<usize> {
    if core::hint::unlikely(len != size_of::<RobustListHead>()) {
        return Err(KernelError::InvalidValue);
    }

    let task = ctx.task_mut();
    task.robust_list.replace(head);

    Ok(0)
}
