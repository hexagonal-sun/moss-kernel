
.section .multiboot_header
.align 4
.long 0x1BADB002                    # Magic
.long 0x00000003                    # Flags (align modules + mem info)
.long -(0x1BADB002 + 0x00000003)    # Checksum
