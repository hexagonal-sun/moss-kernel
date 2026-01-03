use crate::ArchImpl;
use crate::process::Comm;
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
        return exec_with_interp(inode, &elf, endian, &ph_buf, &hdrs, path, argv, envp).await;
    }

    // static ELF ...
    let mut auxv = vec![AT_PHNUM, elf.e_phnum.get(endian) as _, AT_PHENT, elf.e_phentsize(endian) as _];

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

    // Add auxiliary vectors
    auxv.push(AT_PAGESZ);
    auxv.push(PAGE_SIZE as u64);
    auxv.push(AT_RANDOM);
    // TODO: SECURITY: Actually make this a random value.
    auxv.push(STACK_END as u64 - 0x10);
    auxv.push(AT_NULL);
    auxv.push(0);

    info_block.append(&mut auxv);

    let info_block_size = info_block.len() * mem::size_of::<u64>();

    // The top of the info block must be 16-byte aligned. The stack pointer on
    // entry to the new process must also be 16-byte aligned.
    let strings_base_va = STACK_END - total_string_size;
    let final_sp_unaligned = strings_base_va - info_block_size;
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
    let task = current_task();
    let path = Path::new(&interp_path);
    let interp_inode = VFS
        .resolve_path(path, VFS.root_inode(), task.clone())
        .await?;

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

    // Map interpreter at a fixed high base address.
    let interp_base = LINKER_BASE;
    let mut interp_entry = 0;

    for hdr in interp_hdrs.iter() {
        if hdr.p_type(iendian) == PT_LOAD {
            let vaddr = interp_base + hdr.p_vaddr(iendian);
            let size = hdr.p_memsz(iendian) as usize;

            let region = VirtMemoryRegion::new(VA::from_value(vaddr as usize), size);
            let kind = VMAreaKind::new_file(
                interp_inode.clone(),
                hdr.p_offset(iendian),
                hdr.p_filesz(iendian),
            );

            let permissions = {
                let mut perms = VMAPermissions {
                    read: false,
                    write: false,
                    execute: false,
                };

                let flags = hdr.p_flags(iendian);
                if flags & elf::PF_R != 0 {
                    perms.read = true;
                }
                if flags & elf::PF_W != 0 {
                    perms.write = true;
                }
                if flags & elf::PF_X != 0 {
                    perms.execute = true;
                }

                perms
            };

            let vma = VMArea::new(region, kind, permissions);
            vmas.push(vma);

            let end = vaddr + hdr.p_memsz(iendian);
            if end > highest_addr {
                highest_addr = end;
            }
        }
    }

    interp_entry = interp_base + interp_elf.e_entry(iendian);

    // Build auxv for dynamic case:
    // - AT_PHDR/AT_PHENT/AT_PHNUM: main executable
    // - AT_ENTRY: main executable entry
    // - AT_BASE: interpreter base address
    let mut auxv = vec![AT_PHNUM, main_elf.e_phnum.get(endian) as _, AT_PHENT, main_elf.e_phentsize(endian) as _];

    // Find PHDR in main binary: assume p_offset == 0 contains headers
    for hdr in main_hdrs.iter() {
        if hdr.p_type(endian) == PT_LOAD && hdr.p_offset(endian) == 0 {
            auxv.push(AT_PHDR);
            auxv.push(hdr.p_vaddr(endian) + main_elf.e_phoff.get(endian));
            break;
        }
    }

    auxv.push(AT_ENTRY);
    auxv.push(main_entry);

    auxv.push(AT_BASE);
    auxv.push(interp_base);

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
    let current_task = current_task();

    if let Some(new_comm) = new_comm {
        *current_task.comm.lock_save_irq() = new_comm;
    }
    *current_task.ctx.lock_save_irq() = Context::from_user_ctx(user_ctx);
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
