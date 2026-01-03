// Load an absolute 64-bit address in a relocatable way
.macro adr_a register, symbol
    movz	\register, #:abs_g3:\symbol
    movk	\register, #:abs_g2_nc:\symbol
    movk	\register, #:abs_g1_nc:\symbol
    movk	\register, #:abs_g0_nc:\symbol
.endm

.macro __save_and_call handler
    // Save general-purpose registers x0–x29
    stp     x0,  x1,  [sp, #(16 * 0)]
    stp     x2,  x3,  [sp, #(16 * 1)]
    stp     x4,  x5,  [sp, #(16 * 2)]
    stp     x6,  x7,  [sp, #(16 * 3)]
    stp     x8,  x9,  [sp, #(16 * 4)]
    stp     x10, x11, [sp, #(16 * 5)]
    stp     x12, x13, [sp, #(16 * 6)]
    stp     x14, x15, [sp, #(16 * 7)]
    stp     x16, x17, [sp, #(16 * 8)]
    stp     x18, x19, [sp, #(16 * 9)]
    stp     x20, x21, [sp, #(16 * 10)]
    stp     x22, x23, [sp, #(16 * 11)]
    stp     x24, x25, [sp, #(16 * 12)]
    stp     x26, x27, [sp, #(16 * 13)]
    stp     x28, x29, [sp, #(16 * 14)]

    // Save system registers and Link Register
    mrs     x1, ELR_EL1
    mrs     x2, SPSR_EL1
    mrs     x3, SP_EL0
    mrs     x4, TPIDR_EL0
    stp     lr,  x1,  [sp, #(16 * 15)]
    stp     x2,  x3,  [sp, #(16 * 16)]
    str     x4,       [sp, #(16 * 17)]

    mov     x0, sp

    // Call handler
    adr_a   x1, \handler
    blr     x1

    // Exit
    b       restore_ctx_and_eret
.endm

.macro vector_handler handler
__vector_\handler:
    sub     sp, sp, #(16 * 18)

    b       __impl_\handler

    .pushsection .vectors.impl, "ax"
__impl_\handler:
    __save_and_call \handler
    .popsection
.endm

.macro kvector_handler handler
__vector_\handler:
    sub     sp, sp, #(16 * 18)

    // Detect stack overflow without clobbering GP registers.
    msr     TPIDR_EL1, x0
    mov     x0, sp
    //TODO: share this const value with Rust.
	tbnz	x0, #15, 0f // #15 = KERNEL_STACK_SHIFT.
    mrs     x0, TPIDR_EL1
    b       __impl_\handler

0:  
    // Stack overflow detected. Switch to the emergency stack and call the
    // handler to (presumably) panic the kernel.
    ldr     x0, =EMERG_STACK_END
    ldr     x0, [x0]
    mov     sp, x0
    sub     sp, sp, #(16 * 18)
    mrs     x0, TPIDR_EL1
    b       __impl_\handler

    .pushsection .vectors.impl, "ax"
__impl_\handler:
    __save_and_call \handler
    .popsection
.endm

.section .vectors, "ax"
.align 12
.global exception_vectors
exception_vectors:
    .org 0x000
    vector_handler el1_sync_sp0
    .org 0x080
    vector_handler el1_irq_sp0
    .org 0x100
    vector_handler el1_fiq_sp0
    .org 0x180
    vector_handler el1_serror_sp0

    .org 0x200
    kvector_handler el1_sync_spx
    .org 0x280
    kvector_handler el1_irq_spx
    .org 0x300
    kvector_handler el1_fiq_spx
    .org 0x380
    kvector_handler el1_serror_spx

    .org 0x400
    vector_handler el0_sync
    .org 0x480
    vector_handler el0_irq
    .org 0x500
    vector_handler el0_fiq
    .org 0x580
    vector_handler el0_serror


// Common exit path
.section .vectors.impl, "ax"
.global restore_ctx_and_eret
restore_ctx_and_eret:
    add     sp, sp, #(0x10 * 18)
    ldp     lr,  x1,  [x0, #(16 * 15)]
    ldp     x2,  x3,  [x0, #(16 * 16)]
    ldr     x4,       [x0, #(16 * 17)]

    msr     ELR_EL1,   x1
    msr     SPSR_EL1,  x2
    msr     SP_EL0,    x3
    msr     TPIDR_EL0, x4

    ldp     x2,  x3,  [x0, #(16 * 1)]
    ldp     x4,  x5,  [x0, #(16 * 2)]
    ldp     x6,  x7,  [x0, #(16 * 3)]
    ldp     x8,  x9,  [x0, #(16 * 4)]
    ldp     x10, x11, [x0, #(16 * 5)]
    ldp     x12, x13, [x0, #(16 * 6)]
    ldp     x14, x15, [x0, #(16 * 7)]
    ldp     x16, x17, [x0, #(16 * 8)]
    ldp     x18, x19, [x0, #(16 * 9)]
    ldp     x20, x21, [x0, #(16 * 10)]
    ldp     x22, x23, [x0, #(16 * 11)]
    ldp     x24, x25, [x0, #(16 * 12)]
    ldp     x26, x27, [x0, #(16 * 13)]
    ldp     x28, x29, [x0, #(16 * 14)]
    ldp     x0,  x1,  [x0, #(16 * 0)]

    eret

.section .text
.global exception_return
exception_return:
    adr_a   x1 restore_ctx_and_eret
    br      x1

