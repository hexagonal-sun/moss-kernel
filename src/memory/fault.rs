use crate::{process::ProcVM, sched::current_task};
use alloc::boxed::Box;
use libkernel::{
    PageInfo, UserAddressSpace,
    error::{KernelError, MapError, Result},
    memory::{address::VA, permissions::PtePermissions, proc_vm::vmarea::AccessKind},
};

use super::{PAGE_ALLOC, page::ClaimedPage};

/// Represents the outcome of a page fault handling attempt.
///
/// This enum is the return type of `handle_demand_fault` and
/// `handle_protection_fault` and communicates to the caller (the exception
/// handler) how the fault was handled and what action to take next.
pub enum FaultResolution {
    /// The page fault was handled successfully and synchronously. The necessary
    /// page has been allocated and mapped. Or, in the case of CoW, the page has
    /// been copied and an owned-writable page has been mapped in its place. The
    /// faulting instruction can now be safely re-executed.
    Resolved,

    /// The memory access was denied. This indicates a segmentation fault: the
    /// process attempted to access a virtual address that is not mapped in any
    /// valid `VMArea`, or the access permissions (read/write/execute) were
    /// violated. The caller should send a `SIGSEGV` signal to the faulting
    /// process.
    Denied,

    /// The fault resolution has been deferred and requires asynchronous work.
    /// This occurs for faults on pages that need to be filled by a blocking
    /// operation, such as reading from a disk. The contained `Future` will
    /// perform the I/O, map the page, and complete the resolution. The caller
    /// is responsible for polling this future and putting the faulting task to
    /// sleep until the future completes.
    Deferred(Box<dyn Future<Output = Result<()>> + 'static + Send>),
}

/// Handle a page fault when a PTE is not present.
pub fn handle_demand_fault(
    vm: &mut ProcVM,
    faulting_addr: VA,
    access_kind: AccessKind,
) -> Result<FaultResolution> {
    let vma = match vm.find_vma_for_fault(faulting_addr, access_kind) {
        Some(vma) => vma,
        None => return Ok(FaultResolution::Denied),
    }
    .clone();

    let mut new_page = ClaimedPage::alloc_zeroed()?;
    let page_va = faulting_addr.page_aligned();

    if let Some(vma_read) = vma.resolve_fault(faulting_addr) {
        Ok(FaultResolution::Deferred(Box::new(async move {
            let pg_buf = &mut new_page.as_slice_mut()
                [vma_read.page_offset..vma_read.page_offset + vma_read.read_len];

            vma_read.inode.read_at(vma_read.file_offset, pg_buf).await?;

            // Since the above may have put the task to sleep, revalidate the
            // VMA access.
            let task = current_task();
            let mut vm = task.vm.lock_save_irq();

            // If the handler in the deferred case is no longer valid. Allow
            // the program to back to user-space without touching the page
            // tables, to restart the fault handler logic from scratch.
            let is_vma_still_valid = vm
                .find_vma_for_fault(faulting_addr, access_kind)
                .is_some_and(|validated_vma| *validated_vma == vma);

            if !is_vma_still_valid {
                return Ok(());
            }

            match vm.mm_mut().address_space_mut().map_page(
                new_page.pa().to_pfn(),
                page_va,
                PtePermissions::from(vma.permissions()),
            ) {
                Ok(_) => {
                    // We mapped our page, leak it for reclimation by the
                    // address-space tear-down code.
                    new_page.leak();

                    Ok(())
                }
                Err(KernelError::MappingError(MapError::AlreadyMapped)) => {
                    // Another CPU mapped the page for us, since we've validated the
                    // VMA is still valid and the same mapping code has been
                    // executed, it's guarenteed that the correct page will have
                    // been mapped by the other CPU.
                    //
                    // Do not leak the page, since it's not going to be used.
                    Ok(())
                }
                e => e,
            }
        })))
    } else {
        // Anonymous mapping, no need to defer.
        match vm.mm_mut().address_space_mut().map_page(
            new_page.pa().to_pfn(),
            page_va,
            vma.permissions().into(),
        ) {
            Ok(()) => {
                // As per logic above.
                new_page.leak();
                Ok(FaultResolution::Resolved)
            }
            Err(KernelError::MappingError(MapError::AlreadyMapped)) => {
                // As per logic above.
                Ok(FaultResolution::Resolved)
            }
            Err(e) => Err(e),
        }
    }
}

/// Handle a page fault when a page is present, but the access kind differ from
/// permissble accessees defined in the PTE, a 'protection' fault.
pub fn handle_protection_fault(
    vm: &mut ProcVM,
    faulting_addr: VA,
    access_kind: AccessKind,
    pg_info: PageInfo,
) -> Result<FaultResolution> {
    // Detect CoW condition.
    if access_kind == AccessKind::Write && pg_info.perms.is_cow() {
        let new_pte_perms = pg_info.perms.from_cow();

        // After handling a CoW fault, the new, writable page table permissions
        // must be consistent with the VMA permissions
        debug_assert!(
            new_pte_perms
                == vm
                    .find_vma_for_fault(faulting_addr, access_kind)
                    .unwrap()
                    .permissions()
                    .into()
        );

        // We are handling a CoW access.  Check the ref count.
        if PAGE_ALLOC
            .get()
            .unwrap()
            .is_allocated_exclusive(pg_info.pfn)
        {
            // Take ownership of the page.
            vm.mm_mut()
                .address_space_mut()
                .protect_range(faulting_addr.page_region(), new_pte_perms)?;

            Ok(FaultResolution::Resolved)
        } else {
            let mut new_page = ClaimedPage::alloc_zeroed()?;

            // Oterwise, copy data from the new page, map it and decrement
            // the refcount on the shared page.
            let src_page = unsafe { ClaimedPage::from_pfn(pg_info.pfn) };

            let src = src_page.as_slice();
            let dst = new_page.as_slice_mut();

            dst.copy_from_slice(src);

            // Remap the existing CoW mapping with the fresh page.
            vm.mm_mut()
                .address_space_mut()
                .remap(faulting_addr, new_page.leak(), new_pte_perms)
                .unwrap();

            Ok(FaultResolution::Resolved)
        }
    } else {
        // Any other protection fault *should* be a segmentation fault. Let's
        // just verify.
        debug_assert!(vm.find_vma_for_fault(faulting_addr, access_kind).is_none());

        Ok(FaultResolution::Denied)
    }
}
