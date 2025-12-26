use core::ffi::c_char;

use libkernel::{
    error::{FsError, KernelError, Result},
    fs::{FileType, path::Path},
    memory::address::TUA,
};

use crate::{
    fs::{VFS, syscalls::at::resolve_at_start_node},
    memory::uaccess::cstr::UserCStr,
    process::fd_table::Fd,
    sched::current_task,
};

// from linux/fcntl.h
const AT_RENAME_NOREPLACE: u32 = 0x0001; // Do not overwrite existing target.
const AT_RENAME_EXCHANGE: u32 = 0x0002; // Atomically exchange the target and source.
const AT_RENAME_WHITEOUT: u32 = 0x0004; // Create a whiteout entry for the old path.

pub async fn sys_renameat(
    old_dirfd: Fd,
    old_path: TUA<c_char>,
    new_dirfd: Fd,
    new_path: TUA<c_char>,
) -> Result<usize> {
    sys_renameat2(old_dirfd, old_path, new_dirfd, new_path, 0).await
}

pub async fn sys_renameat2(
    old_dirfd: Fd,
    old_path: TUA<c_char>,
    new_dirfd: Fd,
    new_path: TUA<c_char>,
    flags: u32,
) -> Result<usize> {
    let no_replace = flags & AT_RENAME_NOREPLACE != 0;
    let exchange = flags & AT_RENAME_EXCHANGE != 0;
    let whiteout = flags & AT_RENAME_WHITEOUT != 0; // TODO: implement whiteout, at some point

    if whiteout {
        return Err(KernelError::InvalidValue);
    }

    if (no_replace || whiteout) && exchange {
        return Err(KernelError::InvalidValue);
    }

    let mut buf = [0; 1024];
    let mut buf2 = [0; 1024];

    let task = current_task();
    let old_path = Path::new(
        UserCStr::from_ptr(old_path)
            .copy_from_user(&mut buf)
            .await?,
    );
    let new_path = Path::new(
        UserCStr::from_ptr(new_path)
            .copy_from_user(&mut buf2)
            .await?,
    );
    let old_name = old_path.file_name().ok_or(FsError::InvalidInput)?;
    let new_name = new_path.file_name().ok_or(FsError::InvalidInput)?;

    let old_start_node = resolve_at_start_node(old_dirfd, old_path).await?;
    let new_start_node = resolve_at_start_node(new_dirfd, new_path).await?;

    let old_parent_inode = if let Some(parent_path) = old_path.parent() {
        VFS.resolve_path(parent_path, old_start_node.clone(), task.clone())
            .await?
    } else {
        old_start_node.clone()
    };

    let new_parent_inode = if let Some(parent_path) = new_path.parent() {
        VFS.resolve_path(parent_path, new_start_node.clone(), task.clone())
            .await?
    } else {
        new_start_node.clone()
    };

    // verify that the parent inodes are directories
    if old_parent_inode.getattr().await?.file_type != FileType::Directory
        || new_parent_inode.getattr().await?.file_type != FileType::Directory
    {
        return Err(FsError::NotADirectory.into());
    }

    let old_inode = old_parent_inode.lookup(old_name).await?;

    if let Ok(new_inode) = new_parent_inode.lookup(new_name).await {
        if no_replace {
            return Err(FsError::AlreadyExists.into());
        } else if new_inode.getattr().await?.file_type == FileType::Directory {
            if !new_inode.dir_is_empty().await? {
                return Err(FsError::AlreadyExists.into());
            } else if old_inode.getattr().await?.file_type != FileType::Directory {
                return Err(FsError::IsADirectory.into());
            }
        } else if old_inode.getattr().await?.file_type == FileType::Directory
            && new_inode.getattr().await?.file_type != FileType::Directory
        {
            return Err(FsError::NotADirectory.into());
        }
    }

    if exchange {
        VFS.exchange(old_parent_inode, old_name, new_parent_inode, new_name)
            .await?;
    } else {
        VFS.rename(
            old_parent_inode,
            old_name,
            new_parent_inode,
            new_name,
            no_replace,
        )
        .await?;
    }

    Ok(0)
}
