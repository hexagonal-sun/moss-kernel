use alloc::sync::Arc;
use libkernel::error::KernelError;
use libkernel::memory::address::TUA;

use crate::kernel::kpipe::KPipe;
use crate::memory::uaccess::{copy_from_user, copy_to_user};
use crate::process::fd_table::Fd;
use crate::sched::current::current_task;

pub async fn sys_copy_file_range(
    fd_in: Fd,
    off_in: TUA<i32>,
    fd_out: Fd,
    off_out: TUA<i32>,
    size: usize,
    flags: u32,
) -> libkernel::error::Result<usize> {
    if flags != 0 {
        return Err(KernelError::InvalidValue);
    }

    if size == 0 {
        return Ok(0);
    }

    let mut in_off: u64 = if off_in.is_null() {
        0
    } else {
        let v = copy_from_user(off_in).await? as i64;
        if v < 0 {
            return Err(KernelError::InvalidValue);
        }
        v as u64
    };

    let mut out_off: u64 = if off_out.is_null() {
        0
    } else {
        let v = copy_from_user(off_out).await? as i64;
        if v < 0 {
            return Err(KernelError::InvalidValue);
        }
        v as u64
    };

    let (reader, writer) = {
        let task = current_task();
        let fds = task.fd_table.lock_save_irq();

        let reader = fds.get(fd_in).ok_or(KernelError::BadFd)?;
        let writer = fds.get(fd_out).ok_or(KernelError::BadFd)?;

        (reader, writer)
    };

    if Arc::ptr_eq(&reader, &writer) {
        return Err(KernelError::InvalidValue);
    }

    // Fast path: both offsets are NULL, so we can splice using each file's
    // internal cursor.
    if in_off == 0 && out_off == 0 {
        let kbuf = KPipe::new()?;

        let (reader_ops, reader_ctx) = &mut *reader.lock().await;
        let (writer_ops, writer_ctx) = &mut *writer.lock().await;

        let mut remaining = size;
        let mut total_written = 0;

        while remaining > 0 {
            let read = match reader_ops.splice_into(reader_ctx, &kbuf, remaining).await {
                Ok(v) => v,
                Err(e) => {
                    return if total_written > 0 {
                        Ok(total_written)
                    } else {
                        Err(e)
                    };
                }
            };

            if read == 0 {
                return Ok(total_written);
            }

            let mut to_write = read;

            while to_write > 0 {
                let written = match writer_ops.splice_from(writer_ctx, &kbuf, to_write).await {
                    Ok(v) => v,
                    Err(e) => {
                        return if total_written > 0 {
                            Ok(total_written)
                        } else {
                            Err(e)
                        };
                    }
                };
                to_write -= written;
                total_written += written;
            }

            remaining -= read;
        }

        return Ok(total_written);
    }

    // Offset path: at least one of the offsets was provided.
    let kpipe = KPipe::new()?;

    let (reader_ops, reader_ctx) = &mut *reader.lock().await;
    let (writer_ops, writer_ctx) = &mut *writer.lock().await;

    // If an offset pointer is NULL, we use (and update) the file cursor in that direction.
    if off_in.is_null() {
        in_off = reader_ctx.pos;
    }
    if off_out.is_null() {
        out_off = writer_ctx.pos;
    }

    let mut remaining = size;
    let mut total_written = 0usize;

    while remaining > 0 {
        let chunk_sz = core::cmp::min(kpipe.capacity().get(), remaining);

        // Read into the pipe using cursor-based splice, but with a temporary seek when
        // explicit offsets are requested.
        if !off_in.is_null() {
            let saved = reader_ctx.pos;
            reader_ctx.pos = in_off;
            let read = match reader_ops.splice_into(reader_ctx, &kpipe, chunk_sz).await {
                Ok(v) => v,
                Err(e) => {
                    reader_ctx.pos = saved;
                    return if total_written > 0 {
                        Ok(total_written)
                    } else {
                        Err(e)
                    };
                }
            };
            reader_ctx.pos = saved;

            if read == 0 {
                break;
            }
            in_off = in_off.saturating_add(read as u64);

            // Write from the pipe similarly, using temporary seek for explicit out offsets.
            let mut to_write = read;
            while to_write > 0 {
                let saved_out = writer_ctx.pos;
                if !off_out.is_null() {
                    writer_ctx.pos = out_off;
                }

                let written = match writer_ops.splice_from(writer_ctx, &kpipe, to_write).await {
                    Ok(v) => v,
                    Err(e) => {
                        writer_ctx.pos = saved_out;
                        return if total_written > 0 {
                            Ok(total_written)
                        } else {
                            Err(e)
                        };
                    }
                };

                if !off_out.is_null() {
                    writer_ctx.pos = saved_out;
                    out_off = out_off.saturating_add(written as u64);
                }

                to_write -= written;
                total_written += written;
            }

            remaining -= read;
        } else {
            // Input uses file cursor. We can splice directly.
            let read = match reader_ops.splice_into(reader_ctx, &kpipe, chunk_sz).await {
                Ok(v) => v,
                Err(e) => {
                    return if total_written > 0 {
                        Ok(total_written)
                    } else {
                        Err(e)
                    };
                }
            };

            if read == 0 {
                break;
            }

            let mut to_write = read;
            while to_write > 0 {
                // Output might use explicit offset; use temporary seek if so.
                let saved_out = writer_ctx.pos;
                if !off_out.is_null() {
                    writer_ctx.pos = out_off;
                }

                let written = match writer_ops.splice_from(writer_ctx, &kpipe, to_write).await {
                    Ok(v) => v,
                    Err(e) => {
                        writer_ctx.pos = saved_out;
                        return if total_written > 0 {
                            Ok(total_written)
                        } else {
                            Err(e)
                        };
                    }
                };

                if !off_out.is_null() {
                    writer_ctx.pos = saved_out;
                    out_off = out_off.saturating_add(written as u64);
                }

                to_write -= written;
                total_written += written;
            }

            remaining -= read;
        }

        // Update user offsets if provided.
        if !off_in.is_null() {
            copy_to_user(off_in, in_off as i32).await?;
        } else {
            // Track cursor for correctness if we modified it (we didn't), but keep in_off in sync.
            in_off = reader_ctx.pos;
        }

        if !off_out.is_null() {
            copy_to_user(off_out, out_off as i32).await?;
        } else {
            out_off = writer_ctx.pos;
        }
    }

    Ok(total_written)
}
