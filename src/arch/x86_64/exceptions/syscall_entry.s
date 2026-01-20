
/* Removed: assembly moved into Rust-included file `syscall_entry.inc.s`.
    This file is kept empty to avoid Cargo assembling a duplicate
    definition of `syscall_entry`. */
    
    mov qword ptr gs:[8], rsp
    mov rsp, qword ptr gs:[0]

    # 3. Build ExceptionState on kernel stack
    sub rsp, 160
    
    # Save registers
    mov qword ptr [rsp + 0], rax
    mov qword ptr [rsp + 8], rbx
    mov qword ptr [rsp + 16], rcx
    mov qword ptr [rsp + 24], rdx
    mov qword ptr [rsp + 32], rsi
    mov qword ptr [rsp + 40], rdi
    mov qword ptr [rsp + 48], rbp
    mov qword ptr [rsp + 56], r8
    mov qword ptr [rsp + 64], r9
    mov qword ptr [rsp + 72], r10
    mov qword ptr [rsp + 80], r11
    mov qword ptr [rsp + 88], r12
    mov qword ptr [rsp + 96], r13
    mov qword ptr [rsp + 104], r14
    mov qword ptr [rsp + 112], r15
    
    # CPU saves user RIP in RCX and RFLAGS in R11
    mov qword ptr [rsp + 128], rcx # rip
    mov rax, qword ptr gs:[8]      # user_rsp
    mov qword ptr [rsp + 136], rax
    mov qword ptr [rsp + 144], r11 # rflags

    # 4. Call Rust handler
    mov rdi, rsp # Pass pointer to ExceptionState
    
    # Align stack to 16 bytes for ABI
    and rsp, -16
    call handle_syscall_wrapper

    # 5. Restore registers
    mov rax, qword ptr [rsp + 0]
    mov rbx, qword ptr [rsp + 8]
    # rcx handled by sysret
    mov rdx, qword ptr [rsp + 24]
    mov rsi, qword ptr [rsp + 32]
    mov rdi, qword ptr [rsp + 40]
    mov rbp, qword ptr [rsp + 48]
    mov r8, qword ptr [rsp + 56]
    mov r9, qword ptr [rsp + 64]
    mov r10, qword ptr [rsp + 72]
    # r11 handled by sysret
    mov r12, qword ptr [rsp + 88]
    mov r13, qword ptr [rsp + 96]
    mov r14, qword ptr [rsp + 104]
    mov r15, qword ptr [rsp + 112]

    # Restore RIP and RFLAGS for sysret
    mov rcx, qword ptr [rsp + 128]
    mov r11, qword ptr [rsp + 144]
    
    # Restore User RSP
    mov rsp, qword ptr [rsp + 136]

    # 6. Swap GS back and return
    swapgs
    sysretq
