use libkernel::{
    error::{KernelError, Result},
    proc::{
        caps::CapabilitiesFlags,
        ids::{Gid, Uid},
    },
};

use crate::{process::fd_table::Fd, sched::current_task};

pub async fn sys_fchown(fd: Fd, owner: i32, group: i32) -> Result<usize> {
    let task = current_task();
    let file = task
        .fd_table
        .lock_save_irq()
        .get(fd)
        .ok_or(KernelError::BadFd)?;

    let inode = file.inode().ok_or(KernelError::BadFd)?;
    let mut attr = inode.getattr().await?;

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
    inode.setattr(attr).await?;

    Ok(0)
}
