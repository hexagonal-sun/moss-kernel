use crate::arch::{
    Arch, ArchImpl, EMERG_STACK_END, IMAGE_BASE, KERNEL_STACK_AREA, KERNEL_STACK_PG_ORDER,
};
use core::slice;
use libkernel::StackTrace;
use libkernel::memory::{PAGE_SIZE, address::VA, region::VirtMemoryRegion};
use log::error;
#[cfg(target_pointer_width = "32")]
use object::elf::FileHeader32 as FileHeader;
#[cfg(target_pointer_width = "64")]
use object::elf::FileHeader64 as FileHeader;
use object::{
    NativeEndian, elf,
    read::elf::{FileHeader as _, Sym as _},
};

const MAX_FRAMES: usize = 64;

fn emergency_stack_area() -> VirtMemoryRegion {
    let stack_size = PAGE_SIZE << KERNEL_STACK_PG_ORDER;
    VirtMemoryRegion::new(EMERG_STACK_END.sub_bytes(stack_size), stack_size)
}

fn is_valid_frame_pointer<T: StackTrace>(frame: &T) -> bool {
    let fp_virt = VA::from_value(frame.fp());
    let pc_virt = VA::from_value(frame.pc_ptr().addr());
    let emergency_stack = emergency_stack_area();

    let in_kernel_stack =
        |va| KERNEL_STACK_AREA.contains_address(va) || emergency_stack.contains_address(va);

    in_kernel_stack(fp_virt)
        && in_kernel_stack(pc_virt)
        && (frame.fp() as *const usize).is_aligned()
        && frame.pc_ptr().is_aligned()
}

pub fn print_backtrace() {
    unsafe {
        let kernel_ptr = IMAGE_BASE.cast::<u8>().as_ptr();
        let elf_header: &FileHeader<NativeEndian> = object::pod::from_bytes(slice::from_raw_parts(
            kernel_ptr,
            size_of::<FileHeader<NativeEndian>>(),
        ))
        .unwrap()
        .0;

        // This assumes that the linker places .shstrtab as last section. If it
        // isn't, that just causes a recursive panic, not UB.
        let kernel_size = elf_header.e_shoff(NativeEndian) as usize
            + usize::from(elf_header.e_shnum(NativeEndian))
                * usize::from(elf_header.e_shentsize(NativeEndian));
        let kernel_slice = slice::from_raw_parts(kernel_ptr, kernel_size);

        let symbols = elf_header
            .sections(NativeEndian, kernel_slice)
            .unwrap()
            .symbols(NativeEndian, kernel_slice, elf::SHT_SYMTAB)
            .unwrap();

        let mut frame = ArchImpl::stack_trace();

        for _ in 0..MAX_FRAMES {
            let Some(frame_) = frame else {
                break;
            };

            if !is_valid_frame_pointer(&frame_) {
                error!("  {:>016x}: INVALID FRAME", frame_.fp);
                break;
            }

            let pc = *frame_.pc_ptr;
            if pc == 0 {
                error!(" {:>016x}: EMPTY RETURN", frame_.fp);
                break;
            }

            error!("  FP {:>016x}: PC {:>016x}", frame_.fp, pc);

            for sym in symbols.iter() {
                if sym.st_type() != elf::STT_FUNC {
                    continue;
                }
                let sym_addr = sym.st_value.get(NativeEndian) as usize;
                if !(pc >= sym_addr && pc < sym_addr + sym.st_size.get(NativeEndian) as usize) {
                    continue;
                }

                let sym_offset = pc - sym_addr;
                if let Some(sym_name) = sym
                    .name(NativeEndian, symbols.strings())
                    .ok()
                    .and_then(|name| core::str::from_utf8(name).ok())
                {
                    error!("    {sym_name} @ {sym_addr:>016X}+{sym_offset:>04X}");
                } else {
                    error!("    {sym_addr:>016X}+{sym_offset:>04X}");
                }
            }
            frame = frame_.next();
        }
    }
}
