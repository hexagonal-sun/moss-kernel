use crate::{kernel::kpipe::KPipe, process::fd_table::Fd, sched::current_task};
use alloc::sync::Arc;
use libkernel::{
    error::{KernelError, Result},
    memory::address::TUA,
};

pub async fn sys_sendfile(
    out_fd: Fd,
    in_fd: Fd,
    _offset: TUA<u64>,
    mut count: usize,
) -> Result<usize> {
    let (reader, writer) = {
        let task = current_task();
        let fds = task.fd_table.lock_save_irq();

        let reader = fds.get_file(in_fd).ok_or(KernelError::BadFd)?;
        let writer = fds.get_file(out_fd).ok_or(KernelError::BadFd)?;

        (reader, writer)
    };

    if Arc::ptr_eq(&reader, &writer) {
        return Err(KernelError::InvalidValue);
    }

    let kbuf = KPipe::new()?;

    let (reader_ops, reader_ctx) = &mut *reader.lock().await;
    let (writer_ops, writer_ctx) = &mut *writer.lock().await;

    let mut total_written = 0;

    while count > 0 {
        let read = reader_ops.splice_into(reader_ctx, &kbuf, count).await?;

        if read == 0 {
            return Ok(total_written);
        }

        let mut to_write = read;

        while to_write > 0 {
            let written = writer_ops.splice_from(writer_ctx, &kbuf, to_write).await?;
            to_write -= written;
            total_written += written;
        }

        count -= read;
    }

    Ok(total_written)
}
