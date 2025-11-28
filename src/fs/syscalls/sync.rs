use crate::fs::VFS;

pub async fn sys_sync() -> libkernel::error::Result<usize> {
    VFS.sync_all().await?;
    Ok(0)
}
