use core::ffi::c_long;

use crate::sched::current_task;
use libkernel::{
    error::{KernelError, Result},
    memory::address::{TUA, VA},
};

pub async fn sys_set_tid_address(_tidptr: VA) -> Result<usize> {
    let tid = current_task().tid;

    // TODO: implement threading and this system call properly. For now, we just
    // return the PID as the thread id.
    Ok(tid.value() as _)
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

pub async fn sys_set_robust_list(head: TUA<RobustListHead>, len: usize) -> Result<usize> {
    if core::hint::unlikely(len != size_of::<RobustListHead>()) {
        return Err(KernelError::InvalidValue);
    }

    let task = current_task();
    task.robust_list.lock_save_irq().replace(head);

    Ok(0)
}
