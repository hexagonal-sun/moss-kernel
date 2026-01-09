PAGE_SIZE = 4K;
PAGE_MASK = PAGE_SIZE - 1;

ENTRY(_start)
SECTIONS
{
    /DISCARD/ : {
        *(.interp .dynamic)
        *(.dynsym .dynstr .hash .gnu.hash)
   }

   . = 0xffff800000000000;

    ASSERT((. & PAGE_MASK) == 0, "Kernel VA not page aligned")

    __image_start = .;
    __text_start = .;
    .text.boot : {
       KEEP(*(.text.boot._start))
       *(.text.boot)
    }
    .text : { *(.text*) }
    __text_end = .;

    .data : { *(.data*) }
    .rodata : {
        *(.rodata*)
        __driver_inits_start = .;
        *(.driver_inits*)
        __driver_inits_end = .;
        __fixups_start = .;
        *(.exception_fixups*)
        __fixups_end = .;
    }

    .exception_fixups : ALIGN(8) {
    }

    .percpu : ALIGN(8) {
        __percpu_start = .;
        KEEP(*(.percpu))
        __percpu_end = .;
    }

    /*
     * Vectors are here in the binary, but they might be remapped virtually
     * later. `.vectors` and `.vectors.impl` must remain contiguous.
     */
    . = ALIGN(PAGE_SIZE);
    .vectors : {
        __vectors_start = .;
        KEEP(*(.vectors))
        *(.vectors.impl)
        __vectors_end = .;
    }

    . = ALIGN(PAGE_SIZE);
    .vdso : {
        __vdso_start = .;
        KEEP(*(.vdso))
        __vdso_end = .;
    }

    .bss :
    {
        . = ALIGN(16);
        __bss_start = .;
        *(.bss*)
        . = ALIGN(16);
        __bss_end = .;
    }

    . = ALIGN(PAGE_SIZE);
    __init_pages_start = .;
    . += PAGE_SIZE * 64;
    __init_pages_end = .;

    . += 4K;
    __boot_stack = .;
    __image_end = .;

    /DISCARD/ : { *(.comment*) }
}
