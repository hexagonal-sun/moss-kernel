use crate::{process::fd_table::Fd, sched::current_task};
use libkernel::{
    error::{KernelError, Result},
    fs::SeekFrom,
};

const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;
const SEEK_END: i32 = 2;

pub async fn sys_lseek(fd: Fd, offset: isize, whence: i32) -> Result<usize> {
    let seek_from = match whence {
        SEEK_SET => SeekFrom::Start(offset as _),
        SEEK_CUR => SeekFrom::Current(offset as _),
        SEEK_END => SeekFrom::End(offset as _),
        _ => return Err(KernelError::InvalidValue),
    };

    let fd = current_task()
        .fd_table
        .lock_save_irq()
        .get_file(fd)
        .ok_or(KernelError::BadFd)?;

    let (ops, ctx) = &mut *fd.lock().await;

    ops.seek(ctx, seek_from).await.map(|x| x as _)
}
