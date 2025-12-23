use core::ffi::c_long;
use core::mem::size_of;

use crate::sched::current_task;
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

pub mod futex;

pub fn sys_set_tid_address(tidptr: TUA<u32>) -> Result<usize> {
    let task = current_task();

    *task.child_tid_ptr.lock_save_irq() = Some(tidptr);

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

pub async fn sys_set_robust_list(head: TUA<RobustListHead>, len: usize) -> Result<usize> {
    if core::hint::unlikely(len != size_of::<RobustListHead>()) {
        return Err(KernelError::InvalidValue);
    }

    let task = current_task();
    task.robust_list.lock_save_irq().replace(head);

    Ok(0)
}
