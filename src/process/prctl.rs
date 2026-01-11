use crate::memory::uaccess::copy_to_user_slice;
use crate::memory::uaccess::cstr::UserCStr;
use crate::process::Comm;
use crate::sched::current::current_task_shared;
use core::ffi::c_char;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::TUA;

const PR_CAPBSET_READ: i32 = 23;
const PR_SET_NAME: i32 = 15;
const PR_GET_NAME: i32 = 16;
const CAP_MAX: usize = 40;

fn pr_read_capset(what: usize) -> Result<usize> {
    // Validate the argument
    if what > CAP_MAX {
        return Err(KernelError::InvalidValue);
    }

    // Assume we have *all* the capabilities.
    Ok(1)
}

async fn pr_get_name(str: TUA<c_char>) -> Result<usize> {
    let task = current_task_shared();
    let comm = task.comm.lock_save_irq().0;
    copy_to_user_slice(&comm, str.to_untyped()).await?;
    Ok(0)
}

async fn pr_set_name(str: TUA<c_char>) -> Result<usize> {
    let task = current_task_shared();
    let mut buf: [u8; 64] = [0; 64];
    let name = UserCStr::from_ptr(str).copy_from_user(&mut buf).await?;
    *task.comm.lock_save_irq() = Comm::new(name);
    Ok(0)
}

pub async fn sys_prctl(op: i32, arg1: u64) -> Result<usize> {
    match op {
        PR_SET_NAME => pr_set_name(TUA::from_value(arg1 as usize)).await,
        PR_GET_NAME => pr_get_name(TUA::from_value(arg1 as usize)).await,
        PR_CAPBSET_READ => pr_read_capset(arg1 as usize),
        _ => todo!("prctl op: {}", op),
    }
}
