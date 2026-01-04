use crate::ArchImpl;
use crate::process::{Comm, TaskState};
use crate::sched::current::current_task_shared;
use crate::{
    arch::Arch,
    fs::VFS,
    memory::{
        page::ClaimedPage,
        uaccess::{copy_from_user, cstr::UserCStr},
    },
    process::{ctx::Context, thread_group::signal::SignalState},
    sched::current::current_task,
};
use alloc::{string::String, vec};
use alloc::{string::ToString, sync::Arc, vec::Vec};
use auxv::{AT_BASE, AT_ENTRY, AT_NULL, AT_PAGESZ, AT_PHDR, AT_PHENT, AT_PHNUM, AT_RANDOM};
use core::{ffi::c_char, mem, slice};
use libkernel::{
    UserAddressSpace, VirtualMemory,
    error::{ExecError, KernelError, Result},
    fs::{Inode, path::Path},
    memory::{
        PAGE_SIZE,
        address::{TUA, VA},
        permissions::PtePermissions,
        proc_vm::{
            ProcessVM,
            memory_map::MemoryMap,
            vmarea::{VMAPermissions, VMArea, VMAreaKind},
        },
        region::VirtMemoryRegion,
    },
};
use object::{
    LittleEndian,
    elf::{self, PT_LOAD},
    read::elf::{FileHeader, ProgramHeader},
};

mod auxv;

const LINKER_BASE: u64 = 0x0000_7000_0000_0000;

const STACK_END: usize = 0x0000_8000_0000_0000;
const STACK_SZ: usize = 0x2000 * 0x400;
const STACK_START: usize = STACK_END - STACK_SZ;

pub async fn kernel_exec(
    inode: Arc<dyn Inode>,
    argv: Vec<String>,
    envp: Vec<String>,
) -> Result<()> {
    // Read ELF header
    let mut buf = [0u8; core::mem::size_of::<elf::FileHeader64<LittleEndian>>()];
    inode.read_at(0, &mut buf).await?;

    let elf = elf::FileHeader64::<LittleEndian>::parse(buf.as_slice())
        .map_err(|_| ExecError::InvalidElfFormat)?;
    let endian = elf.endian().unwrap();

    // Read full program header table
    let ph_table_size = elf.e_phnum.get(endian) as usize * elf.e_phentsize.get(endian) as usize
        + elf.e_phoff.get(endian) as usize;
    let mut ph_buf = vec![0u8; ph_table_size];

    inode.read_at(0, &mut ph_buf).await?;

    let hdrs = elf
        .program_headers(endian, ph_buf.as_slice())
        .map_err(|_| ExecError::InvalidPHdrFormat)?;

    // Detect PT_INTERP (dynamic linker) if present
    let mut interp_path: Option<String> = None;
    for hdr in hdrs.iter() {
        if hdr.p_type(endian) == elf::PT_INTERP {
            let off = hdr.p_offset(endian) as usize;
            let filesz = hdr.p_filesz(endian) as usize;
            if filesz == 0 {
                break;
            }

            let mut ibuf = vec![0u8; filesz];
            inode.read_at(off as u64, &mut ibuf).await?;

            let len = ibuf.iter().position(|&b| b == 0).unwrap_or(filesz);
            let s = core::str::from_utf8(&ibuf[..len]).map_err(|_| ExecError::InvalidElfFormat)?;
            interp_path = Some(s.to_string());
            break;
        }
    }

    if let Some(path) = interp_path {
        return exec_with_interp(inode, elf, endian, &ph_buf, hdrs, path, argv, envp).await;
    }

    // static ELF ...
    let mut auxv = vec![
        AT_PHNUM,
        elf.e_phnum.get(endian) as _,
        AT_PHENT,
        elf.e_phentsize(endian) as _,
    ];

    let mut vmas = Vec::new();
    let mut highest_addr = 0;

    for hdr in hdrs {
        let kind = hdr.p_type(endian);

        if kind == PT_LOAD {
            vmas.push(VMArea::from_pheader(inode.clone(), *hdr, endian));

            if hdr.p_offset.get(endian) == 0 {
                // TODO: potentially more validation that this VA will contain
                // the program headers.
                auxv.push(AT_PHDR);
                auxv.push(hdr.p_vaddr.get(endian) + elf.e_phoff.get(endian));
            }

            let mapping_end = hdr.p_vaddr(endian) + hdr.p_memsz(endian);

            if mapping_end > highest_addr {
                highest_addr = mapping_end;
            }
        }
    }

    auxv.push(AT_ENTRY);
    auxv.push(elf.e_entry(endian));

    vmas.push(VMArea::new(
        VirtMemoryRegion::new(VA::from_value(STACK_START), STACK_SZ),
        VMAreaKind::Anon,
        VMAPermissions::rw(),
    ));

    let mut mem_map = MemoryMap::from_vmas(vmas)?;

    let stack_ptr = setup_user_stack(&mut mem_map, &argv, &envp, auxv)?;

    let user_ctx =
        ArchImpl::new_user_context(VA::from_value(elf.e_entry(endian) as usize), stack_ptr);
    let mut vm = ProcessVM::from_map(mem_map, VA::from_value(highest_addr as usize));

    // We don't have to worry about actually calling for a full context switch
    // here. Parts of the old process that are replaced will go out of scope and
    // be cleaned up (open files, etc); We don't need to preseve any extra
    // state. Simply activate the new process's address space.
    vm.mm_mut().address_space_mut().activate();

    let new_comm = argv.first().map(|s| Comm::new(s.as_str()));

    let mut current_task = current_task();

    if let Some(new_comm) = new_comm {
        *current_task.comm.lock_save_irq() = new_comm;
    }

    current_task.ctx = Context::from_user_ctx(user_ctx);
    *current_task.vm.lock_save_irq() = vm;
    *current_task.process.signals.lock_save_irq() = SignalState::new_default();

    Ok(())
}

// Sets up the user stack according to the System V ABI.
//
// The stack layout from high addresses to low addresses is:
// - Argument and Environment strings
// - Padding to 16-byte boundary
// - Auxiliary Vector (auxv)
// - Environment pointers (envp)
// - Argument pointers (argv)
// - Argument count (argc)
//
// The final stack pointer will point to `argc`.
fn setup_user_stack(
    mm: &mut MemoryMap<<ArchImpl as VirtualMemory>::ProcessAddressSpace>,
    argv: &[String],
    envp: &[String],
    mut auxv: Vec<u64>,
) -> Result<VA> {
    // Calculate the space needed and the virtual addresses for all strings and
    // pointers.
    let mut string_addrs = Vec::new();
    let mut total_string_size = 0;

    // We add strings to the stack from top-down.
    for s in envp.iter().chain(argv.iter()) {
        let len = s.len() + 1; // +1 for null terminator
        total_string_size += len;
        string_addrs.push(len); // Temporarily store length
    }

    let mut current_va = STACK_END;
    for len in string_addrs.iter_mut().rev() {
        // Now calculate the final virtual address of each string.
        current_va -= *len;
        *len = current_va; // Replace length with the VA
    }

    let (envp_addrs, argv_addrs) = string_addrs.split_at(envp.len());

    let mut info_block = Vec::<u64>::new();
    info_block.push(argv.len() as u64); // argc
    info_block.extend(argv_addrs.iter().map(|&addr| addr as u64));
    info_block.push(0); // Null terminator for argv
    info_block.extend(envp_addrs.iter().map(|&addr| addr as u64));
    info_block.push(0); // Null terminator for envp

    // Prepare 16 random bytes for AT_RANDOM and place them just below strings.
    let mut at_random_bytes = [0u8; 16];
    // TODO: Better randomness
    {
        let mut seed: u64 = 0;
        seed ^= (STACK_END as u64).wrapping_mul(0x9E3779B185EBCA87);
        seed ^= (STACK_START as u64).wrapping_mul(0xC2B2AE3D27D4EB4F);
        for i in 0..16 {
            let val = seed.wrapping_mul(STACK_END as u64);
            at_random_bytes[i] = (val >> ((i % 8) * 8)) as u8;
        }
    }

    // Add auxiliary vectors
    auxv.push(AT_PAGESZ);
    auxv.push(PAGE_SIZE as u64);
    auxv.push(AT_RANDOM);
    // Placeholder; will be overwritten below.
    auxv.push(0);
    auxv.push(AT_NULL);
    auxv.push(0);

    // Compute sizes for info block to maintain alignment.

    // The top of the info block must be 16-byte aligned. The stack pointer on
    // entry to the new process must also be 16-byte aligned.
    let strings_base_va = STACK_END - total_string_size;

    // Place the 16 random bytes immediately below the strings region to avoid overlapping.
    let at_random_va = strings_base_va - 16;

    if let Some(pos) = auxv.iter().position(|&v| v == AT_RANDOM) {
        if pos + 1 < auxv.len() {
            auxv[pos + 1] = at_random_va as u64;
        }
    }

    // Append auxv after argc/argv/envp in info_block.
    info_block.append(&mut auxv);

    let info_block_size = info_block.len() * mem::size_of::<u64>();

    let final_sp_unaligned = strings_base_va - 16 /* AT_RANDOM bytes */ - info_block_size;
    let final_sp_val = final_sp_unaligned & !0xF; // Align down to 16 bytes

    let total_stack_size = STACK_END - final_sp_val;
    if total_stack_size > STACK_SZ {
        return Err(KernelError::TooLarge);
    }

    let mut stack_image = vec![0u8; total_stack_size];

    // Write strings into the image
    let mut string_cursor = STACK_END;
    for s in envp.iter().chain(argv.iter()).rev() {
        string_cursor -= s.len() + 1;
        let offset = total_stack_size - (STACK_END - string_cursor);
        stack_image[offset..offset + s.len()].copy_from_slice(s.as_bytes());
        // Null terminator is already there from vec![0;...].
    }

    // Write the random bytes at at_random_va
    {
        let offset = total_stack_size - (STACK_END - at_random_va);
        stack_image[offset..offset + 16].copy_from_slice(&at_random_bytes);
    }

    // Write info block into the image
    let info_block_bytes: &[u8] =
        unsafe { slice::from_raw_parts(info_block.as_ptr().cast(), info_block_size) };
    let info_block_offset = total_stack_size - (STACK_END - final_sp_val);
    stack_image[info_block_offset..info_block_offset + info_block_size]
        .copy_from_slice(info_block_bytes);

    // Allocate pages, copy image, and map into user space
    let num_pages = total_stack_size.div_ceil(PAGE_SIZE);

    for i in 0..num_pages {
        let mut page = ClaimedPage::alloc_zeroed()?;

        // Calculate the slice of the stack image that corresponds to this page
        let image_end = total_stack_size - i * PAGE_SIZE;
        let image_start = image_end.saturating_sub(PAGE_SIZE);
        let image_slice = &stack_image[image_start..image_end];

        // Copy the data
        let page_slice = page.as_slice_mut();
        page_slice[PAGE_SIZE - image_slice.len()..].copy_from_slice(image_slice);

        // Map the page to the correct virtual address
        let page_va = VA::from_value(STACK_END - (i + 1) * PAGE_SIZE);
        mm.address_space_mut()
            .map_page(page.leak(), page_va, PtePermissions::rw(true))?;
    }

    Ok(VA::from_value(final_sp_val))
}

// Dynamic linker path: map main executable and its PT_INTERP interpreter and
// start execution at the interpreter entry point.
#[expect(clippy::too_many_arguments)]
async fn exec_with_interp(
    inode_main: Arc<dyn Inode>,
    main_elf: &elf::FileHeader64<LittleEndian>,
    endian: LittleEndian,
    _main_ph_buf: &[u8],
    main_hdrs: &[elf::ProgramHeader64<LittleEndian>],
    interp_path: String,
    argv: Vec<String>,
    envp: Vec<String>,
) -> Result<()> {
    // Resolve interpreter path from root; this assumes interp_path is absolute.
    let task = current_task_shared();
    let path = Path::new(&interp_path);
    let interp_inode = VFS.resolve_path(path, VFS.root_inode(), &task).await?;
    // Debug: interpreter path
    log::info!("PT_INTERP path: {}", interp_path);

    // Parse interpreter ELF header
    let mut hdr_buf = [0u8; core::mem::size_of::<elf::FileHeader64<LittleEndian>>()];
    interp_inode.read_at(0, &mut hdr_buf).await?;
    let interp_elf = elf::FileHeader64::<LittleEndian>::parse(&hdr_buf[..])
        .map_err(|_| ExecError::InvalidElfFormat)?;
    let iendian = interp_elf.endian().unwrap();

    // Read interpreter program headers
    let interp_ph_table_size = interp_elf.e_phnum.get(iendian) as usize
        * interp_elf.e_phentsize.get(iendian) as usize
        + interp_elf.e_phoff.get(iendian) as usize;
    let mut interp_ph_buf = vec![0u8; interp_ph_table_size];
    interp_inode.read_at(0, &mut interp_ph_buf).await?;
    let interp_hdrs = interp_elf
        .program_headers(iendian, &interp_ph_buf[..])
        .map_err(|_| ExecError::InvalidPHdrFormat)?;

    // Build VMAs for main executable and interpreter
    let mut vmas = Vec::new();
    let mut highest_addr = 0u64;

    // Map main executable segments (identity-mapped as in static case)
    for hdr in main_hdrs.iter() {
        if hdr.p_type(endian) == PT_LOAD {
            vmas.push(VMArea::from_pheader(inode_main.clone(), *hdr, endian));

            let end = hdr.p_vaddr(endian) + hdr.p_memsz(endian);
            if end > highest_addr {
                highest_addr = end;
            }
        }
    }

    let main_entry = main_elf.e_entry(endian);

    // Map interpreter at a fixed high base address
    let interp_base = LINKER_BASE;
    log::info!("Interp base: 0x{:016x}", interp_base);
    let mut interp_entry = 0;

    for hdr in interp_hdrs.iter() {
        if hdr.p_type(iendian) == PT_LOAD {
            let seg_vaddr = hdr.p_vaddr(iendian);
            let seg_offset = hdr.p_offset(iendian);
            let filesz = hdr.p_filesz(iendian);
            let memsz = hdr.p_memsz(iendian);
            log::info!(
                "Interp PT_LOAD: off=0x{:x} vaddr=0x{:x} filesz=0x{:x} memsz=0x{:x} flags=0x{:x}",
                seg_offset,
                seg_vaddr,
                filesz,
                memsz,
                hdr.p_flags(iendian)
            );

            let page_mask = !(PAGE_SIZE as u64 - 1);
            let map_start_file_off = seg_offset & page_mask;
            let map_start_vaddr = (interp_base + seg_vaddr) & page_mask;
            let file_backed_end_vaddr = ((interp_base + seg_vaddr + filesz) + (PAGE_SIZE as u64 - 1)) & page_mask; // round up
            let mem_end_vaddr = ((interp_base + seg_vaddr + memsz) + (PAGE_SIZE as u64 - 1)) & page_mask; // round up
            log::info!(
                "map file: file_off=0x{:x} va=0x{:x} -> 0x{:x} (len=0x{:x})",
                map_start_file_off,
                map_start_vaddr,
                file_backed_end_vaddr,
                (file_backed_end_vaddr - map_start_vaddr)
            );
            log::info!(
                "mem end (incl. BSS): 0x{:x}",
                mem_end_vaddr
            );

            let file_map_size = (file_backed_end_vaddr - map_start_vaddr) as usize;
            if file_map_size > 0 {
                // Exact file-backed length in VA space: leading partial page + filesz
                let leading = (seg_offset - map_start_file_off) as u64;
                let file_va_len_exact = (filesz + leading) as usize;
                let file_va_end_exact = map_start_vaddr as usize + file_va_len_exact;

                // File-backed region: [map_start_vaddr, file_va_end_exact)
                if file_va_len_exact > 0 {
                    let region = VirtMemoryRegion::new(
                        VA::from_value(map_start_vaddr as usize),
                        file_va_len_exact,
                    );
                    let kind = VMAreaKind::new_file(
                        interp_inode.clone(),
                        map_start_file_off,
                        (filesz + leading) as u64,
                    );
                    let permissions = {
                        let mut perms = VMAPermissions { read: false, write: false, execute: false };
                        let flags = hdr.p_flags(iendian);
                        if flags & elf::PF_R != 0 { perms.read = true; }
                        if flags & elf::PF_W != 0 { perms.write = true; }
                        if flags & elf::PF_X != 0 { perms.execute = true; }
                        perms
                    };
                    log::info!(
                        "file VMA exact: VA[0x{:x}..0x{:x}) perms=r{}w{}x{} back_len=0x{:x}",
                        map_start_vaddr,
                        file_va_end_exact,
                        if permissions.read {1}else{0},
                        if permissions.write {1}else{0},
                        if permissions.execute {1}else{0},
                        (filesz + leading)
                    );
                    vmas.push(VMArea::new(region, kind, permissions));
                }

                // If the exact file-backed end is before the rounded-up end, add anon zero-fill for the remainder of the page.
                if (file_va_end_exact as u64) < file_backed_end_vaddr {
                    let region = VirtMemoryRegion::new(
                        VA::from_value(file_va_end_exact),
                        (file_backed_end_vaddr - file_va_end_exact as u64) as usize,
                    );
                    let permissions = {
                        let mut perms = VMAPermissions { read: false, write: false, execute: false };
                        let flags = hdr.p_flags(iendian);
                        if flags & elf::PF_R != 0 { perms.read = true; }
                        if flags & elf::PF_W != 0 { perms.write = true; }
                        if flags & elf::PF_X != 0 { perms.execute = true; }
                        perms
                    };
                    log::info!(
                        "file-tail anon VMA: VA[0x{:x}..0x{:x}) perms=r{}w{}x{}",
                        file_va_end_exact,
                        file_backed_end_vaddr,
                        if permissions.read {1}else{0},
                        if permissions.write {1}else{0},
                        if permissions.execute {1}else{0}
                    );
                    vmas.push(VMArea::new(region, VMAreaKind::Anon, permissions));
                }
            }

            // If there is a BSS tail (memsz > filesz), map an anonymous zero-filled region.
            if memsz > filesz {
                let bss_start = ((interp_base + seg_vaddr + filesz) + (PAGE_SIZE as u64 - 1)) & page_mask; // start at next page boundary
                let bss_size = (mem_end_vaddr - bss_start) as usize;
                if bss_size > 0 {
                    let region = VirtMemoryRegion::new(VA::from_value(bss_start as usize), bss_size);
                    let permissions = {
                        let mut perms = VMAPermissions { read: false, write: false, execute: false };
                        let flags = hdr.p_flags(iendian);
                        if flags & elf::PF_R != 0 { perms.read = true; }
                        if flags & elf::PF_W != 0 { perms.write = true; }
                        if flags & elf::PF_X != 0 { perms.execute = true; }
                        perms
                    };
                    log::info!(
                        "BSS VMA: VA[0x{:x}..0x{:x}) perms=r{}w{}x{}",
                        bss_start,
                        mem_end_vaddr,
                        if permissions.read {1}else{0},
                        if permissions.write {1}else{0},
                        if permissions.execute {1}else{0}
                    );
                    vmas.push(VMArea::new(region, VMAreaKind::Anon, permissions));
                }
            }

            let end = mem_end_vaddr;
            if end > highest_addr {
                highest_addr = end;
            }
        }
    }

    interp_entry = interp_base + interp_elf.e_entry(iendian);
    log::info!("Interp entry: 0x{:x} (e_entry=0x{:x})", interp_entry, interp_elf.e_entry(iendian));

    let mut auxv = vec![
        AT_PHNUM,
        main_elf.e_phnum.get(endian) as _,
        AT_PHENT,
        main_elf.e_phentsize(endian) as _,
    ];

    for hdr in main_hdrs.iter() {
        if hdr.p_type(endian) == PT_LOAD && hdr.p_offset(endian) == 0 {
            let at_phdr_va = hdr.p_vaddr(endian) + main_elf.e_phoff.get(endian);
            log::info!(
                "AT_PHDR: VA=0x{:x} (vaddr=0x{:x} e_phoff=0x{:x})",
                at_phdr_va,
                hdr.p_vaddr(endian),
                main_elf.e_phoff.get(endian)
            );
            auxv.push(AT_PHDR);
            auxv.push(at_phdr_va);
            break;
        }
    }

    auxv.push(AT_ENTRY);
    auxv.push(main_entry);
    log::info!("AT_ENTRY: 0x{:x}", main_entry);

    auxv.push(AT_BASE);
    auxv.push(interp_base);
    log::info!("AT_BASE: 0x{:x}", interp_base);

    // Stack VMA
    vmas.push(VMArea::new(
        VirtMemoryRegion::new(VA::from_value(STACK_START), STACK_SZ),
        VMAreaKind::Anon,
        VMAPermissions::rw(),
    ));

    let mut mem_map = MemoryMap::from_vmas(vmas)?;
    let stack_ptr = setup_user_stack(&mut mem_map, &argv, &envp, auxv)?;

    // Start execution at interpreter entry
    let entry_va = VA::from_value(interp_entry as usize);
    let user_ctx = ArchImpl::new_user_context(entry_va, stack_ptr);

    let mut vm = ProcessVM::from_map(mem_map, VA::from_value(highest_addr as usize));
    vm.mm_mut().address_space_mut().activate();

    let new_comm = argv.first().map(|s| Comm::new(s.as_str()));
    let mut current_task = current_task();

    if let Some(new_comm) = new_comm {
        *current_task.comm.lock_save_irq() = new_comm;
    }
    current_task.ctx = Context::from_user_ctx(user_ctx);
    *current_task.state.lock_save_irq() = TaskState::Runnable;
    *current_task.vm.lock_save_irq() = vm;
    *current_task.process.signals.lock_save_irq() = SignalState::new_default();

    Ok(())
}

pub async fn sys_execve(
    path: TUA<c_char>,
    mut usr_argv: TUA<TUA<c_char>>,
    mut usr_env: TUA<TUA<c_char>>,
) -> Result<usize> {
    let task = current_task_shared();
    let mut buf = [0; 1024];
    let mut argv = Vec::new();
    let mut envp = Vec::new();

    loop {
        let ptr = copy_from_user(usr_argv).await?;

        if ptr.is_null() {
            break;
        }

        let str = UserCStr::from_ptr(ptr).copy_from_user(&mut buf).await?;
        argv.push(str.to_string());
        usr_argv = usr_argv.add_objs(1);
    }

    loop {
        let ptr = copy_from_user(usr_env).await?;

        if ptr.is_null() {
            break;
        }

        let str = UserCStr::from_ptr(ptr).copy_from_user(&mut buf).await?;
        envp.push(str.to_string());
        usr_env = usr_env.add_objs(1);
    }

    let path = Path::new(UserCStr::from_ptr(path).copy_from_user(&mut buf).await?);
    let inode = VFS.resolve_path(path, VFS.root_inode(), &task).await?;

    kernel_exec(inode, argv, envp).await?;

    Ok(0)
}
