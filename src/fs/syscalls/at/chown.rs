use core::ffi::c_char;

use libkernel::{
    error::Result,
    fs::path::Path,
    memory::address::TUA,
    proc::{
        caps::CapabilitiesFlags,
        ids::{Gid, Uid},
    },
};

use crate::{
    fs::syscalls::at::{AtFlags, resolve_at_start_node, resolve_path_flags},
    memory::uaccess::cstr::UserCStr,
    process::fd_table::Fd,
    sched::current_task,
};

pub async fn sys_fchownat(
    dirfd: Fd,
    path: TUA<c_char>,
    owner: i32,
    group: i32,
    flags: i32,
) -> Result<usize> {
    let mut buf = [0; 1024];

    let task = current_task();
    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let start_node = resolve_at_start_node(dirfd, path).await?;
    let flags = AtFlags::from_bits_retain(flags);

    let node = resolve_path_flags(dirfd, path, start_node, task.clone(), flags).await?;
    let mut attr = node.getattr().await?;

    {
        let creds = task.creds.lock_save_irq();
        if owner != -1 {
            creds.caps().check_capable(CapabilitiesFlags::CAP_CHOWN)?;
            attr.uid = Uid::new(owner as _);
        }
        if group != -1 {
            let gid = Gid::new(group as _);
            // doesnt seem like theres real groups so this is as good as it gets
            if creds.uid() != attr.uid || creds.gid() != gid {
                creds.caps().check_capable(CapabilitiesFlags::CAP_CHOWN)?;
            }
            attr.gid = gid;
        }
    }
    node.setattr(attr).await?;

    Ok(0)
}
