.section .vdso, "ax"
.global sig_return
sig_return:
    mov     x8, #0x8b // sys_sgreturn
    svc     #0
