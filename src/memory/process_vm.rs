use core::{cmp::min, slice};

use super::{
    PageOffsetTranslator,
    uaccess::{copy_obj_array_from_user, copy_to_user_slice},
};
use crate::process::Tid;
use crate::{fs::syscalls::iov::IoVec, process::thread_group::pid::PidT};
use libkernel::{
    error::{KernelError, Result},
    memory::{PAGE_SIZE, address::TUA, proc_vm::vmarea::AccessKind},
};

pub async fn sys_process_vm_readv(
    ctx: &crate::sched::syscall_ctx::ProcessCtx,
    pid: PidT,
    local_iov: TUA<IoVec>,
    liov_count: usize,
    remote_iov: TUA<IoVec>,
    riov_count: usize,
    flags: usize,
) -> Result<usize> {
    if flags != 0 || liov_count > 1024 || riov_count > 1024 {
        return Err(KernelError::InvalidValue);
    }
    let tgid = Tid::from_pid_t(pid);
    let remote_proc = crate::process::pid_namespace::find_task(ctx.shared(), tgid.0)
        .ok_or(KernelError::NoProcess)?;
    crate::process::access::ptrace_may_access(ctx.shared(), &remote_proc, false)?;
    let local_iovs = copy_obj_array_from_user(local_iov, liov_count).await?;
    let remote_iovs = copy_obj_array_from_user(remote_iov, riov_count).await?;

    let mut total_bytes_copied = 0;

    let mut local_iov_idx = 0;
    let mut local_iov_curr_offset = 0;

    let mut remote_iov_idx = 0;
    let mut remote_iov_curr_offset = 0;

    while let Some(remote_iov) = remote_iovs.get(remote_iov_idx)
        && let Some(local_iov) = local_iovs.get(local_iov_idx)
    {
        let remote_remaining = remote_iov.iov_len - remote_iov_curr_offset;
        let local_remaining = local_iov.iov_len - local_iov_curr_offset;

        let remote_va = remote_iov.iov_base.add_bytes(remote_iov_curr_offset);

        let chunk_sz = min(
            PAGE_SIZE - remote_va.page_offset(),
            min(remote_remaining, local_remaining),
        );

        // If we have nothing left to copy in current vectors, advance them.
        if chunk_sz == 0 {
            if remote_remaining == 0 {
                remote_iov_idx += 1;
                remote_iov_curr_offset = 0;
            }

            if local_remaining == 0 {
                local_iov_idx += 1;
                local_iov_curr_offset = 0;
            }

            continue;
        }

        let copy_result = async {
            // Get the page (pins it)
            // SAFETY: We only read.
            let remote_page = unsafe { remote_proc.get_page(remote_va, AccessKind::Read).await? };

            // Map physical page to kernel virtual address (Direct Map)
            let remote_pg_slice = unsafe {
                slice::from_raw_parts(
                    remote_page
                        .region()
                        .start_address()
                        .to_va::<PageOffsetTranslator>()
                        .cast::<u8>()
                        .add_bytes(remote_va.page_offset())
                        .as_ptr(),
                    chunk_sz,
                )
            };

            // Copy to local user memory
            copy_to_user_slice(
                remote_pg_slice,
                local_iov.iov_base.add_bytes(local_iov_curr_offset),
            )
            .await
        }
        .await;

        match copy_result {
            Ok(_) => {
                total_bytes_copied += chunk_sz;
                remote_iov_curr_offset += chunk_sz;
                local_iov_curr_offset += chunk_sz;
            }
            Err(e) => {
                if total_bytes_copied > 0 {
                    // Partial success: return what we got so far.
                    return Ok(total_bytes_copied);
                } else {
                    // No data copied at all: return the error.
                    return Err(e);
                }
            }
        }
    }

    Ok(total_bytes_copied)
}
