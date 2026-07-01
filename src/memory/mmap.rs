use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{process::fd_table::Fd, sched::syscall_ctx::ProcessCtx};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use libkernel::{
    error::{KernelError, Result},
    memory::{
        address::VA,
        proc_vm::{
            memory_map::{AddressRequest, RemapDestination},
            vmarea::{VMAPermissions, VMAreaKind},
        },
        region::VirtMemoryRegion,
    },
};

const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;

const MAP_SHARED: u64 = 0x0001;
const MAP_PRIVATE: u64 = 0x0002;
const MAP_FIXED: u64 = 0x0010;
const MAP_FIXED_NOREPLACE: u64 = 0x100000;
const MAP_ANON: u64 = 0x0020;
const MAP_ANONYMOUS: u64 = 0x0020;

const MREMAP_MAYMOVE: u64 = 1;
const MREMAP_FIXED: u64 = 2;
const MREMAP_DONTUNMAP: u64 = 4;

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
    ctx: &ProcessCtx,
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

    let (kind, name) = if (flags & (MAP_ANON | MAP_ANONYMOUS)) != 0 {
        (VMAreaKind::Anon, String::new())
    } else {
        // File-backed mapping: require a valid fd and use the provided offset.
        let fd = ctx
            .shared()
            .fd_table
            .lock_save_irq()
            .get(fd)
            .ok_or(KernelError::BadFd)?;

        let inode = fd.inode().ok_or(KernelError::BadFd)?;
        let name = fd
            .path()
            .map(|x| x.as_str().to_string())
            .unwrap_or_default();

        (VMAreaKind::new_file(inode, offset, len), name)
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
    let new_mapping_addr = ctx.shared().vm.lock_save_irq().mm_mut().mmap(
        address_request,
        requested_len,
        permissions,
        kind,
        name,
    )?;

    Ok(new_mapping_addr.value())
}

fn free_unmapped_pages(pages: Vec<libkernel::memory::page::PageFrame>) -> Result<()> {
    if pages.is_empty() {
        return Ok(());
    }

    // The frames returned by unmap/mremap are no longer mapped and belong to this process;
    // creating temporary allocations from these regions allows the allocator to reclaim them on drop.
    let allocator = crate::memory::PAGE_ALLOC
        .get()
        .ok_or(KernelError::NoMemory)?;

    // Create a temporary allocation from the single-page region and drop it immediately to free.
    for p in pages {
        let tmp = unsafe { allocator.alloc_from_region(p.as_phys_range()) };
        drop(tmp);
    }

    Ok(())
}

pub async fn sys_munmap(ctx: &ProcessCtx, addr: VA, len: usize) -> Result<usize> {
    let region = VirtMemoryRegion::new(addr, len);
    let pages = ctx.shared().vm.lock_save_irq().mm_mut().munmap(region)?;

    free_unmapped_pages(pages)?;

    Ok(0)
}

pub async fn sys_mremap(
    ctx: &ProcessCtx,
    old_addr: VA,
    old_len: usize,
    new_len: usize,
    flags: u64,
    new_addr: VA,
) -> Result<usize> {
    if flags & !(MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP) != 0 {
        return Err(KernelError::InvalidValue);
    }

    if old_len == 0 {
        // Linux only allows this special case for shareable mappings, which moss does not support yet.
        return Err(KernelError::InvalidValue);
    }

    if new_len == 0 || !old_addr.is_page_aligned() {
        return Err(KernelError::InvalidValue);
    }

    if (flags & MREMAP_DONTUNMAP) != 0 {
        return Err(KernelError::InvalidValue);
    }

    let destination = if (flags & MREMAP_FIXED) != 0 {
        if (flags & MREMAP_MAYMOVE) == 0 || !new_addr.is_page_aligned() {
            return Err(KernelError::InvalidValue);
        }

        let old_region = VirtMemoryRegion::new(old_addr, old_len).align_to_page_boundary();
        let new_region = VirtMemoryRegion::new(new_addr, new_len).align_to_page_boundary();

        if old_region.overlaps(new_region) {
            return Err(KernelError::InvalidValue);
        }

        RemapDestination::Fixed(new_addr)
    } else if (flags & MREMAP_MAYMOVE) != 0 {
        RemapDestination::MayMove
    } else {
        RemapDestination::InPlaceOnly
    };

    let (new_mapping_addr, pages_to_free) =
        ctx.shared()
            .vm
            .lock_save_irq()
            .mm_mut()
            .mremap(old_addr, old_len, new_len, destination)?;

    free_unmapped_pages(pages_to_free)?;

    Ok(new_mapping_addr.value())
}

pub fn sys_mprotect(ctx: &ProcessCtx, addr: VA, len: usize, prot: u64) -> Result<usize> {
    let perms = prot_to_perms(prot);
    let region = VirtMemoryRegion::new(addr, len);

    ctx.shared()
        .vm
        .lock_save_irq()
        .mm_mut()
        .mprotect(region, perms)?;

    Ok(0)
}
