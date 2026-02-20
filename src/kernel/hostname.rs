use crate::memory::uaccess::copy_from_user_slice;
use crate::sched::current::current_task_shared;
use crate::sync::OnceLock;
use crate::sync::SpinLock;
use alloc::string::{String, ToString};
use alloc::vec;
use core::ffi::c_char;
use libkernel::error::{KernelError, Result};
use libkernel::memory::address::TUA;
use libkernel::proc::caps::CapabilitiesFlags;

static HOSTNAME: OnceLock<SpinLock<String>> = OnceLock::new();

pub fn hostname() -> &'static SpinLock<String> {
    HOSTNAME.get_or_init(|| SpinLock::new(String::from("moss-machine")))
}

const HOST_NAME_MAX: usize = 64;

pub async fn sys_sethostname(name_ptr: TUA<c_char>, name_len: usize) -> Result<usize> {
    {
        let task = current_task_shared();
        let creds = task.creds.lock_save_irq();
        creds
            .caps()
            .check_capable(CapabilitiesFlags::CAP_SYS_ADMIN)?;
    }

    if name_len > HOST_NAME_MAX {
        return Err(KernelError::NameTooLong);
    }
    let mut buf = vec![0u8; name_len];
    copy_from_user_slice(name_ptr.to_untyped(), &mut buf).await?;
    let name = core::str::from_utf8(&buf)
        .map_err(|_| KernelError::InvalidValue)?
        .trim_end_matches('\0');
    *hostname().lock_save_irq() = name.to_string();
    Ok(0)
}

// pub async fn sys_gethostname(name_ptr: TUA<c_char>, name_len: usize) -> Result<usize> {
//     let hostname = hostname().lock_save_irq();
//     let bytes = hostname.as_bytes();
//     let len = core::cmp::min(bytes.len(), name_len);
//     copy_to_user_slice(&bytes[..len], name_ptr.to_untyped()).await?;
//     // Null-terminate if there's space
//     if name_len > len {
//         copy_to_user(name_ptr.add_bytes(len), 0u8).await?;
//     }
//     Ok(0)
// }
