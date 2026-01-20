
.section .text.boot, "ax"
.global start
.global p4_table
.global p3_table
.global p2_table
.global stack_top
.global gdt64
.global gdt64_pointer

.code32

start:
    # PRESERVE MULTIBOOT INFO
    mov esi, ebx # Use esi to store multiboot info pointer
    # SERIAL DEBUG: 'S' (Start)
    mov dx, 0x3f8
    mov al, 0x53
    out dx, al

    # 1. Zero page tables
    mov edi, offset p4_table
    mov ecx, 4096 * 3 / 4
    xor eax, eax
    rep stosd

    # 2. Map P4[0] -> P3
    mov eax, offset p3_table
    or eax, 3
    mov ebx, offset p4_table
    mov dword ptr [ebx], eax

    # 3. Map P3[0] -> P2
    mov eax, offset p2_table
    or eax, 3
    mov ebx, offset p3_table
    mov dword ptr [ebx], eax

    # 4. Identity map P2 (1GB)
    mov ecx, 0
    mov ebx, offset p2_table
.map_p2_table:
    mov eax, 0x200000 
    mul ecx 
    or eax, 0x83 
    
    mov dword ptr [ebx + ecx*8], eax
    mov dword ptr [ebx + ecx*8 + 4], 0 

    inc ecx
    cmp ecx, 512
    jne .map_p2_table

    # 5. Enable PAE
    mov eax, cr4
    or eax, (1 << 5)
    mov cr4, eax

    # 6. Load CR3
    mov eax, offset p4_table
    mov cr3, eax

    # 7. Enable Long Mode
    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8)
    wrmsr

    # 8. Enable Paging
    mov eax, cr0
    or eax, (1 << 31)
    mov cr0, eax

    # 8b. Enable SSE
    mov eax, cr0
    and ax, 0xFFFB      # clear coprocessor emulation CR0.EM
    or ax, 0x2          # set coprocessor monitoring  CR0.MP
    mov cr0, eax
    mov eax, cr4
    or ax, 3 << 9       # set CR4.OSFXSR and CR4.OSXMMEXCPT
    mov cr4, eax

    # SERIAL DEBUG: 'G' (GDT)
    mov dx, 0x3f8
    mov al, 0x47
    out dx, al

    # 9. Load GDT (PC-relative displacement if possible, or just absolute)
    mov eax, offset gdt64_pointer
    lgdt [eax]

    # Stack Setup
    mov esp, offset stack_top

    # SERIAL DEBUG: 'J' (Jump)
    mov dx, 0x3f8
    mov al, 0x4a
    out dx, al

    # 10. Jump to long mode
    push 0x08
    mov eax, offset long_mode_start
    push eax
    retf

.code64
long_mode_start:
    # SERIAL DEBUG: 'L' (Long mode)
    mov dx, 0x3f8
    mov al, 0x4c
    out dx, al

    xor ax, ax
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    mov rdi, rsi # restore multiboot info from rsi
    call x86_64_start
    hlt

.align 8
gdt64:
    .quad 0
    .quad 0x00af9a000000ffff # Code segment
gdt64_pointer:
    .word 15
    .long gdt64

.align 4096
p4_table:
    .skip 4096
p3_table:
    .skip 4096
p2_table:
    .skip 4096
stack_bottom:
    .skip 65536
stack_top:
