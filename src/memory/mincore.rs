use alloc::vec;
use alloc::vec::Vec;

use crate::memory::uaccess::copy_to_user_slice;
use crate::sched::syscall_ctx::ProcessCtx;
use libkernel::memory::region::VirtMemoryRegion;
use libkernel::{
    UserAddressSpace,
    error::{KernelError, Result},
    memory::PAGE_SHIFT,
    memory::address::{UA, VA},
};

pub async fn sys_mincore(ctx: &ProcessCtx, start: u64, len: usize, vec: UA) -> Result<usize> {
    // addr must be a multiple of the system page size
    // len must be > 0
    let start_va = VA::from_value(start as usize);
    if !start_va.is_page_aligned() {
        return Err(KernelError::InvalidValue);
    }

    if len == 0 {
        return Err(KernelError::InvalidValue);
    }

    let region = VirtMemoryRegion::new(start_va, len)
        .to_mappable_region()
        .region();
    if region.size() == 0 {
        return Err(KernelError::InvalidValue);
    }

    // Vector length must be number of pages covering the region
    let pages = region.size() >> PAGE_SHIFT;
    let mut buf: Vec<u8> = vec![0; pages];

    {
        let mut vm_guard = ctx.shared().vm.lock_save_irq();
        let mm = vm_guard.mm_mut();

        // Validate the entire region is covered by VMAs
        for va in region.iter_pages() {
            if mm.find_vma(va).is_none() {
                return Err(KernelError::NoMemory);
            }
        }

        let as_ref = mm.address_space_mut();

        for (i, va) in region.iter_pages().enumerate() {
            let resident = as_ref.translate(va).is_some();
            if resident {
                buf[i] |= 1;
            } else {
                buf[i] &= !1;
            }
        }
    }

    copy_to_user_slice(&buf, vec)
        .await
        .map_err(|_| KernelError::Fault)?;

    Ok(0)
}
