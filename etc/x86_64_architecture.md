# x86_64 Architecture Implementation

This document describes the x86_64 architecture implementation for the mtIX kernel.

## Overview

The x86_64 port provides a complete 64-bit kernel implementation targeting modern Intel/AMD processors. It supports multiboot2 loading, virtual memory management, interrupt handling, and device drivers for x86_64 hardware platforms.

## Key Features

### Boot Process
- **Multiboot2 Compliance**: Uses GRUB multiboot2 standard for boot loading
- **Memory Detection**: Automatically detects available RAM via multiboot info
- **CPU Initialization**: Sets up long mode, paging, and kernel segments
- **ACPI Support**: Reads ACPI tables for hardware enumeration

### Memory Management
- **4-level Page Tables**: Implements standard x86_64 4KB paging
- **Kernel Space Mapping**: Direct mapping of kernel space (upper 2GB)
- **User Space Protection**: Separate address spaces with proper permissions
- **Heap Management**: Buddy allocator for physical memory management

### Interrupt Handling
- **IDT Setup**: 256-entry interrupt descriptor table
- **Exception Handling**: Full x86_64 exception support (divide by zero, page fault, etc.)
- **External IRQs**: PIC/IO-APIC support for device interrupts
- **System Calls**: Fast syscall interface via `syscall` instruction

### Device Drivers
- **Serial Console**: 16550 UART for debug output
- **VGA Text Mode**: Basic framebuffer text console
- **PCI Enumeration**: PCI bus scanning for device discovery
- **VirtIO Drivers**: Para-virtualized drivers for QEMU environment
  - VirtIO Block Device
  - VirtIO Network Device
  - VirtIO Console

### File Systems
- **Ext4 Support**: Full ext4 filesystem implementation
- **Initrd Support**: Loads initial ramdisk from multiboot modules
- **Virtual File System**: Unified VFS layer with mount points

## Build and Run

### Prerequisites
```bash
# Install required tools
sudo apt install build-essential gcc nasm qemu-system-x86 grub-pc-bin

# Install Rust with x86_64-unknown-none target
rustup target add x86_64-unknown-none
```

### Building
```bash
# Build kernel and user programs
cargo build --target x86_64-unknown-none

# Create filesystem image (no root required)
./scripts/create-image.sh

# Run in QEMU
./scripts/qemu-runner-x86_64.sh
```

### QEMU Command
```bash
qemu-system-x86_64 \
    -kernel target/x86_64-unknown-none/debug/moss32 \
    -initrd moss.img \
    -m 512M \
    -serial stdio \
    -display none \
    -no-reboot \
    -no-shutdown \
    -append "--init /bin/usertest --rootfs ext4fs --automount /dev,devfs"
```

## Memory Layout

### Virtual Memory
```
0xFFFFFFFF80000000 - 0xFFFFFFFFFFFFFFFF : Kernel direct mapping
0x00007FFFFFFFFFFF - 0xFFFFFFFF7FFFFFFF : User space
0x0000000000000000 - 0x00007FFFFFFFFFFF : Kernel space
```

### Physical Memory
- Identity mapped for bootloader and early boot
- Kernel heap managed by buddy allocator
- User allocations via slab allocators

## Porting Notes

### Architecture Dependencies
- **CPU**: Requires x86_64 with long mode support
- **MMU**: Hardware paging required
- **Minimum**: Intel/AMD 64-bit CPU, 512MB RAM

### Key Files
```
src/arch/x86_64/
├── boot.S          # Assembly boot entry point
├── mod.rs          # Architecture module definition
├── cpu.rs          # CPU detection and setup
├── memory.rs       # Memory management
├── interrupts.rs   # IDT and exception handling
├── acpi.rs         # ACPI table parsing
└── devices.rs      # Platform-specific devices

libkernel/src/arch/x86_64/
├── paging.rs       # Page table management
├── gdt.rs          # Global descriptor table
├── idt.rs          # Interrupt descriptor table
└── mod.rs          # Architecture traits
```

### Performance Considerations
- **TLB Usage**: Optimized page table walks
- **Cache Alignment**: Structures aligned to cache lines
- **Interrupt Latency**: Fast syscall interface
- **Memory Bandwidth**: Efficient copy routines

## Debugging

### Serial Output
Use `-serial stdio` QEMU flag to see kernel logs:
```bash
qemu-system-x86_64 -serial mon:stdio
```

### GDB Support
Enable GDB debugging:
```bash
qemu-system-x86_64 -s -S
gdb target/x86_64-unknown-none/debug/moss32
```

### Common Issues
1. **Page Faults**: Check memory mappings and permissions
2. **Triple Faults**: Usually stack corruption or IDT issues  
3. **Boot Failures**: Verify multiboot2 configuration
4. **Driver Issues**: Check PCI device initialization

## Compatibility

### Tested Platforms
- QEMU 6.0+ (x86_64 system mode)
- Intel Core i-series CPUs
- AMD Ryzen CPUs

### Guest Environment
- **Hypervisor**: QEMU/KVM recommended
- **Devices**: VirtIO for best performance
- **Memory**: Minimum 512MB, recommended 2GB+

## Future Enhancements

### Planned Features
- [ ] SMP Support (multi-core)
- [ ] APIC/x2APIC interrupt handling
- [ ] USB device support
- [ ] Network stack (TCP/IP)
- [ ] Graphics framebuffer driver

### Optimization Targets
- [ ] Zero-copy I/O paths
- [ ] Better cache utilization
- [ ] Reduced interrupt latency
- [ ] Memory compression