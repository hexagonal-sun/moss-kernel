use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{process::fd_table::Fd, sched::current::current_task};
use libkernel::{
    error::{KernelError, Result},
    memory::{
        address::VA,
        proc_vm::{
            memory_map::AddressRequest,
            vmarea::{VMAPermissions, VMAreaKind},
        },
        region::VirtMemoryRegion,
    },
};

const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;

const MAP_FILE: u64 = 0x0000;
const MAP_SHARED: u64 = 0x0001;
const MAP_PRIVATE: u64 = 0x0002;
const MAP_FIXED: u64 = 0x0010;
const MAP_FIXED_NOREPLACE: u64 = 0x100000;
const MAP_ANON: u64 = 0x0020;
const MAP_ANONYMOUS: u64 = 0x0020;

/// Determines the minimal address that user-space is allowed to specify for
/// MAP_FIXED{,_NOREPLACE}.
static MMAP_MIN_ADDR: AtomicUsize = AtomicUsize::new(0x1000);

fn prot_to_perms(prot: u64) -> VMAPermissions {
    VMAPermissions {
        read: (prot & PROT_READ) != 0,
        write: (prot & PROT_WRITE) != 0,
        execute: (prot & PROT_EXEC) != 0,
    }
}

/// Handles the `mmap` system call.
///
/// # Arguments
/// The raw arguments from the syscall registers, corresponding to mmap(2).
///
/// # Returns
/// A `Result` containing the starting address of the new mapping on success,
/// or a `KernelError` on failure.
pub async fn sys_mmap(
    addr: u64,
    len: u64,
    prot: u64,
    flags: u64,
    fd: Fd,
    offset: u64,
) -> Result<usize> {
    if len == 0 {
        return Err(KernelError::InvalidValue);
    }

    // Ensure mapping sharability has been specified:
    if (flags & (MAP_SHARED | MAP_PRIVATE)) == 0 {
        return Err(KernelError::InvalidValue);
    }

    // TODO: Shared Mappings.
    if (flags & MAP_SHARED) != 0 {
        return Err(KernelError::NotSupported);
    }

    // `MAP_FIXED` and `MAP_FIXED_NOREPLACE` are mutually exclusive.
    if (flags & MAP_FIXED) != 0 && (flags & MAP_FIXED_NOREPLACE) != 0 {
        return Err(KernelError::InvalidValue);
    }

    let addr = VA::from_value(addr as usize);

    // If `MAP_FIXED` or `MAP_FIXED_NOREPLACE` is specified, the address must be
    // page-aligned and > MMAP_MIN_ADDR.
    if (flags & (MAP_FIXED | MAP_FIXED_NOREPLACE)) != 0
        && (!addr.is_page_aligned() || addr < VA::from_value(MMAP_MIN_ADDR.load(Ordering::SeqCst)))
    {
        return Err(KernelError::InvalidValue);
    }

    let permissions = prot_to_perms(prot);

    let requested_len = len as usize;

    let kind = if flags & (MAP_ANON | MAP_ANONYMOUS) != 0 {
        VMAreaKind::Anon
    } else if flags == MAP_FILE {
        let fd = current_task()
            .fd_table
            .lock_save_irq()
            .get(fd)
            .ok_or(KernelError::BadFd)?;

        let inode = fd.inode().ok_or(KernelError::BadFd)?;

        VMAreaKind::new_file(inode, offset, len)
    } else {
        // One of MAP_FILE or MAP_ANONYMOUS must be set.
        return Err(KernelError::InvalidValue);
    };

    let address_request = if addr.is_null() {
        AddressRequest::Any
    } else if (flags & MAP_FIXED_NOREPLACE) != 0 {
        AddressRequest::Fixed {
            address: addr,
            permit_overlap: false,
        }
    } else if (flags & MAP_FIXED) != 0 {
        // MAP_FIXED: Map at this exact address, destroying any existing
        // mappings in that range.
        AddressRequest::Fixed {
            address: addr,
            permit_overlap: true,
        }
    } else {
        // No MAP_FIXED flags: The provided address is just a hint.
        AddressRequest::Hint(addr)
    };

    // Lock the task and call the core memory manager to perform the mapping.
    let new_mapping_addr = current_task().vm.lock_save_irq().mm_mut().mmap(
        address_request,
        requested_len,
        permissions,
        kind,
    )?;

    Ok(new_mapping_addr.value())
}

pub async fn sys_munmap(addr: VA, len: usize) -> Result<usize> {
    let region = VirtMemoryRegion::new(addr, len);

    let pages = current_task().vm.lock_save_irq().mm_mut().munmap(region)?;

    // Free any physical frames that were unmapped.
    if !pages.is_empty() {
        // The frames returned by munmap are no longer mapped and belong to this process;
        // creating temporary allocations from these regions allows the allocator to reclaim them on drop.
        let allocator = crate::memory::PAGE_ALLOC
            .get()
            .ok_or(KernelError::NoMemory)?;

        for p in pages {
            // Create a temporary allocation from the single-page region and drop it immediately to free.
            let tmp = unsafe { allocator.alloc_from_region(p.as_phys_range()) };
            drop(tmp);
        }
    }

    Ok(0)
}

pub fn sys_mprotect(addr: VA, len: usize, prot: u64) -> Result<usize> {
    let perms = prot_to_perms(prot);
    let region = VirtMemoryRegion::new(addr, len);

    current_task()
        .vm
        .lock_save_irq()
        .mm_mut()
        .mprotect(region, perms)?;

    Ok(0)
}
