use crate::{fs::VFS, memory::uaccess::cstr::UserCStr, sched::syscall_ctx::ProcessCtx};
use alloc::sync::Arc;
use core::ffi::c_char;
use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{FileType, Inode, InodeId, path::Path},
    memory::address::TUA,
    proc::caps::CapabilitiesFlags,
};

async fn resolve_attachment_inode(
    ctx: &ProcessCtx,
    path: &Path,
) -> Result<(Arc<dyn Inode>, InodeId)> {
    let task = ctx.shared().clone();
    let cwd = task.cwd.lock_save_irq().0.clone();
    let resolved = VFS.resolve_path(path, cwd.clone(), &task).await?;

    let attachment = if let Some(name) = path.file_name() {
        let parent = if let Some(parent_path) = path.parent() {
            VFS.resolve_path(parent_path, cwd, &task).await?
        } else if path.is_absolute() {
            task.root.lock_save_irq().0.clone()
        } else {
            cwd
        };

        parent.lookup(name).await?.id()
    } else if let Some(mount_point) = VFS.mount_point_for_root(resolved.id()) {
        // This supports mount-root paths like "." when they already resolve to
        // the root of a mounted filesystem.
        mount_point
    } else {
        resolved.id()
    };

    Ok((resolved, attachment))
}

async fn path_is_descendant_or_same(
    mut path: Arc<dyn Inode>,
    ancestor: Arc<dyn Inode>,
) -> Result<bool> {
    loop {
        if path.id() == ancestor.id() {
            return Ok(true);
        }

        let parent = path.lookup("..").await?;
        if parent.id() == path.id() {
            return Ok(false);
        }

        path = parent;
    }
}

pub async fn sys_pivot_root(
    ctx: &ProcessCtx,
    new_root: TUA<c_char>,
    put_old: TUA<c_char>,
) -> Result<usize> {
    let task = ctx.shared().clone();
    task.creds
        .lock_save_irq()
        .caps()
        .check_capable(CapabilitiesFlags::CAP_SYS_ADMIN)?;

    let old_root = task.root.lock_save_irq().0.clone();
    if !VFS.is_mount_root(old_root.id()) {
        return Err(KernelError::InvalidValue);
    }

    let mut buf = [0u8; 1024];
    let new_root = Path::new(
        UserCStr::from_ptr(new_root)
            .copy_from_user(&mut buf)
            .await?,
    );

    let mut buf = [0u8; 1024];
    let put_old = Path::new(UserCStr::from_ptr(put_old).copy_from_user(&mut buf).await?);

    let (new_root_inode, new_root_attachment) = resolve_attachment_inode(ctx, new_root).await?;
    let (put_old_inode, put_old_attachment) = resolve_attachment_inode(ctx, put_old).await?;

    if new_root_inode.getattr().await?.file_type != FileType::Directory
        || put_old_inode.getattr().await?.file_type != FileType::Directory
    {
        return Err(FsError::NotADirectory.into());
    }

    if new_root_inode.id().fs_id() == old_root.id().fs_id()
        || put_old_inode.id().fs_id() == old_root.id().fs_id()
    {
        return Err(KernelError::InUse);
    }

    if !VFS.is_mount_root(new_root_inode.id()) {
        return Err(KernelError::InvalidValue);
    }

    if !path_is_descendant_or_same(put_old_inode.clone(), new_root_inode.clone()).await? {
        return Err(KernelError::InvalidValue);
    }

    if VFS.is_mount_point(put_old_attachment) {
        return Err(KernelError::InUse);
    }

    let _ = VFS.pivot_root(new_root_attachment, put_old_attachment)?;

    Ok(0)
}
