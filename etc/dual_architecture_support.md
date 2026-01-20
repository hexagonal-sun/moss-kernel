# Dual Architecture Support in moss

moss kernel has been enhanced to support both **AArch64** and **x86_64** architectures from a unified codebase. This document explains the implementation approach and key differences.

## Architecture Strategy

### Code Organization
```
moss-kernel/
├── src/
│   ├── arch/
│   │   ├── aarch64/          # ARM64-specific implementation
│   │   └── x86_64/          # x86_64-specific implementation
│   └── main.rs               # Architecture-agnostic kernel entry
├── libkernel/
│   ├── src/
│   │   ├── arch/
│   │   │   ├── aarch64/      # ARM64 libkernel traits
│   │   │   └── x86_64/      # x86_64 libkernel traits
│   │   └── ...                # Common kernel code
│   └── Cargo.toml
└── scripts/
    ├── qemu-runner-aarch64.sh
    └── qemu-runner-x86_64.sh
```

### Build Targets
- **AArch64**: `aarch64-unknown-none` (bare metal)
- **x86_64**: `x86_64-unknown-none` (bare metal)

### Common vs Architecture-Specific Code

#### Common Code (libkernel)
- Virtual File System (VFS)
- Filesystem drivers (ext4, fat32, etc.)
- Process management and scheduling
- Syscall implementations
- Memory allocators
- Synchronization primitives

#### Architecture-Specific Code
- Boot sequence and early initialization
- Page table management
- Interrupt handling
- Device drivers (UART, console, etc.)
- Context switching
- System call interface

## Implementation Differences

### Boot Process
| Feature | AArch64 | x86_64 |
|----------|------------|-----------|
| Boot Standard | UEFI + Device Tree | Multiboot2 + GRUB |
| Entry Point | Assembly in `boot.S` | Assembly in `boot.S` |
| Early Console | PL011 UART | 16550 UART |
| Device Info | Device Tree FDT | ACPI Tables + PCI |

### Memory Management
| Feature | AArch64 | x86_64 |
|----------|------------|-----------|
| Page Levels | 3-level | 4-level |
| Page Size | 4KB (configurable) | 4KB |
| Kernel Mapping | Upper 1GB direct | Upper 2GB direct |
| CoW Support | Full copy-on-write | Basic (in development) |
| TLB Management | Hardware-managed | Software hints |

### Interrupt Handling
| Feature | AArch64 | x86_64 |
|----------|------------|-----------|
| Exception Levels | EL0/EL1/EL2/EL3 | Ring 0-3 |
| IRQ Controller | GIC v2/v3 | PIC/IO-APIC |
| Syscall Method | `svc` instruction | `syscall` instruction |
| Fast Path | Via `eret` | Via `sysret` |

### Device Drivers
| Device | AArch64 | x86_64 |
|--------|------------|-----------|
| Console | PL011 UART | 16550 UART |
| Block | VirtIO_blk | VirtIO_blk |
| Network | VirtIO_net | VirtIO_net |
| Display | Simple framebuffer | VGA text mode |
| PCI | N/A | VirtIO + Native |

## Development Workflow

### Building Both Architectures
```bash
# Build for current host architecture
cargo build --release

# Cross-build AArch64 on x86
cargo build --release --target aarch64-unknown-none

# Cross-build x86_64 on AArch64  
cargo build --release --target x86_64-unknown-none
```

### Testing Across Architectures
```bash
# Run libkernel tests on host
cargo test -p libkernel

# Cross-compile and test architecture logic
cargo test -p libkernel --target x86_64-unknown-linux-gnu

# Test all architectures
cargo test
```

### Running Simulators
```bash
# AArch64
./scripts/qemu-runner-aarch64.sh

# x86_64
./scripts/qemu-runner-x86_64.sh
```


## Documentation

- [AArch64 Details](./etc/aarch64_architecture.md)
- [x86_64 Details](./etc/x86_64_architecture.md)
- [AArch64 Syscalls](./etc/syscalls_linux_aarch64.md)
- [x86_64 Syscalls](./etc/syscalls_linux_x86_64.md)
