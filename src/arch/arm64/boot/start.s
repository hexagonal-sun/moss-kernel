.globl _start
.section ".text.boot"

//
// Convenience macros for symbol address resolution
//

// Resolve a symbol's address relative to the current PC (for position-independent code)
.macro adr_r register, symbol
    adrp	\register, \symbol                     // Get page-aligned base of symbol
    add	\register, \register, #:lo12:\symbol   // Add low 12 bits (page offset)
.endm

// Load an absolute 64-bit address in a relocatable way
.macro adr_ab register, symbol
    movz	\register, #:abs_g3:\symbol
    movk	\register, #:abs_g2_nc:\symbol
    movk	\register, #:abs_g1_nc:\symbol
    movk	\register, #:abs_g0_nc:\symbol
.endm

//
// Entry point of the kernel. x0 contains the FDT address as per Linux boot ABI.
//
_start:
    // Linux Kernel Image Header
    nop
    b 1f                          // Branch past header
    .quad   0                     // Text offset (from base of RAM)
    .quad   0                     // Kernel image size
    .quad   0                     // Flags
    .quad   0                     // Reserved
    .quad   0                     // Reserved
    .quad   0                     // Reserved
    .ascii  "ARM\x64"             // Magic value for 64-bit ARM
    .long   0                     // Version

1:  mov     x19, x0               // Save FDT pointer (device tree blob) for later use

    // Zero the .bss section
    adr_r   x1, __bss_start       // Start of .bss
    adr_r   x2, __bss_end         // End of .bss
1:  cmp     x1, x2
    b.eq    2f
    stp     xzr, xzr, [x1], #16
    b       1b

    // Setup temporary stack (before MMU is enabled)
2:  adr_r   x0, __boot_stack      // Address of early boot stack
    mov     sp, x0                // Set stack pointer

    // Sanitize TPIDR_EL1 so Rust allocators know per-cpu data isn't ready.
    msr     tpidr_el1, xzr

    // Transition to EL1 (if not already there)
    // This typically drops from EL2 to EL1 and sets up minimal EL1 state
    bl      transition_to_el1

    // Set up temporary identity + high memory mapping and enable the MMU.
    adr_r   x0, __init_pages_start// Arg 1: Physical address of pg table scratch memory
    adr_r   x1, __image_start     // Arg 2: Physical address of kernel image start
    mov     x2, x19               // Arg 3: FDT address (from earlier)
    bl      paging_bootstrap

    // paging_bootstrap returns the L0 base used in TTBR1
    mov     x3, x0                // Arg 4: Save page table base for later use

    adr_ab  x4, arch_init_stage1  // Absolute address of `arch_init_stage1`
    mov     x0, x19               // Arg 1: FDT pointer
    adr_r   x1, __image_start     // Arg 2: image_start (PA)
    adr_r   x2, __image_end       // Arg 3: image_end (PA)
    adr_ab  lr, 1f                // Keep highmem return address
    br      x4                    // jump to highmem

    // Update SP to the highmem kernel stack returned from stage1
1:  mov     sp, x0

    // Allocate a context switch frame
    sub     sp, sp, #{frame_size}

    // Jump to the main kernel bring-up path
    mov     x0, sp  // context switch frame ptr.
    bl      arch_init_stage2

    // Switch to the task that has been scheduled.
    b       exception_return

    // Just in case.
    b       .
