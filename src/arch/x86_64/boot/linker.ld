ENTRY(start)

SECTIONS {
    . = 1M;

    .text.boot : {
        KEEP(*(.multiboot_header))
        *(.text.boot)
    }

    .text : {
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
        . = ALIGN(8);
        __driver_inits_start = .;
        KEEP(*(.driver_inits))
        __driver_inits_end = .;
    }

    .percpu : {
        __percpu_start = .;
        *(.percpu)
        __percpu_end = .;
    }

    .data : {
        *(.data .data.*)
    }

    .bss : {
        *(.bss .bss.*)
    }
}
