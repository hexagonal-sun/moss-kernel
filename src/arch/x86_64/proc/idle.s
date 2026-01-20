.att_syntax
.section .text
.global __idle_start
.global __idle_end

__idle_start:
    hlt
    jmp __idle_start
__idle_end:
