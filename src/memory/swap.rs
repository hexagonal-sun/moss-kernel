use alloc::vec;
use alloc::vec::Vec;

use crate::memory::uaccess::copy_to_user_slice;
use libkernel::{
    error::{KernelError, Result},
    memory::{
        PAGE_SIZE,
        address::{UA, VA},
    },
};

pub async fn sys_mincore(start: u64, len: usize, vec: UA) -> Result<usize> {
    // addr must be a multiple of the system page size
    // len must be > 0
    let start_va = VA::from_value(start as usize);
    if !start_va.is_page_aligned() {
        return Err(KernelError::InvalidValue);
    }

    if len == 0 {
        return Err(KernelError::InvalidValue);
    }

    // Guard against overflow and obviously bogus ranges.
    let end = start
        .checked_add(len as u64)
        .ok_or(KernelError::InvalidValue)?;
    if end <= start {
        return Err(KernelError::InvalidValue);
    }

    // Compute number of pages covered by [start, start + len).
    let pages = len.div_ceil(PAGE_SIZE);
    if pages == 0 {
        return Err(KernelError::InvalidValue);
    }

    let buf: Vec<u8> = vec![0; pages];

    copy_to_user_slice(&buf, vec)
        .await
        .map_err(|_| KernelError::Fault)?;

    Ok(0)
}
