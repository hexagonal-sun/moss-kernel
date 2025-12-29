use core::ffi::c_char;

use libkernel::{
    error::{KernelError, Result},
    fs::{attr::FilePermissions, path::Path},
    memory::address::TUA,
    proc::{caps::CapabilitiesFlags, ids::Uid},
};
use ringbuf::Arc;

use crate::{
    fs::syscalls::at::{AtFlags, resolve_at_start_node, resolve_path_flags},
    memory::uaccess::cstr::UserCStr,
    process::{Task, fd_table::Fd},
    sched::current_task,
};

pub fn can_chmod(task: Arc<Task>, uid: Uid) -> bool {
    let creds = task.creds.lock_save_irq();
    creds.caps().is_capable(CapabilitiesFlags::CAP_FOWNER) || creds.uid() == uid
}

pub async fn sys_fchmodat(dirfd: Fd, path: TUA<c_char>, mode: u16, flags: i32) -> Result<usize> {
    let flags = AtFlags::from_bits_retain(flags);

    let mut buf = [0; 1024];

    let task = current_task();
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let start_node = resolve_at_start_node(dirfd, path).await?;
    let mode = FilePermissions::from_bits_retain(mode);

    let node = resolve_path_flags(dirfd, path, start_node, task.clone(), flags).await?;
    let mut attr = node.getattr().await?;

    if !can_chmod(task, attr.uid) {
        return Err(KernelError::NotPermitted);
    }

    attr.mode = mode;
    node.setattr(attr).await?;

    Ok(0)
}
