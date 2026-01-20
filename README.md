# moss

![Architecture](https://img.shields.io/badge/arch-aarch64%20%7C%20x86__64-blue)
![Multi-arch](https://img.shields.io/badge/platform-unified-green)
![Language](https://img.shields.io/badge/language-Rust-orange)
![License](https://img.shields.io/badge/license-MIT-yellow)
[![IRC](https://img.shields.io/badge/OFTC_IRC-%23moss-blue)](https://webchat.oftc.net/?nick=&channels=%23moss)

![Moss Boot Demo](etc/moss_demo.gif)

**moss** is a Unix-like, Linux-compatible kernel written in Rust with multi-architecture
support. Initially developed for AArch64, it now includes a complete x86_64 implementation.

It features a modern, asynchronous core, a modular architecture abstraction
layer, and binary compatibility with Linux userspace applications (currently
capable of running most BusyBox commands).

## Features

### Architecture & Memory
* Full support for **AArch64** and **x86_64** architectures.
* A well-defined HAL allowing for easy porting to other architectures (e.g.,
   RISC-V, MIPS).
* Memory Management:
    * Full MMU enablement and page table management.
    * Copy-on-Write (CoW) pages.
    * Safe copy to/from userspace async functions.
    * Kernel and userspace page fault management.
    * Buddy allocator for physical addresses and `smalloc` for boot allocations
      and tracking memory reservations.
* **x86_64 Specific**:
    * 4-level page tables with 4KB pages
    * Long mode and kernel segments setup
    * IDT and exception handling
    * Multiboot2 compliance with GRUB
* **AArch64 Specific**:
    * 3-level page tables with 4KB pages  
    * EL1/EL0 exception levels
    * GIC interrupt controller
    * Device tree parsing

### Async Core
One of the defining features of `moss` is its usage of Rust's `async/await`
model within the kernel context:
* All non-trivial system calls are written as `async` functions, sleep-able
  functions are prefixed with `.await`.
* The compiler enforces that spinlocks cannot be held over sleep points,
  eliminating a common class of kernel deadlocks.

### Process Management
* Full task management including both UP and SMP scheduling via EEVDF and task migration via IPIs.
* Currently implements [85 Linux syscalls](./etc/syscalls_linux_aarch64.md) for AArch64 and [85 Linux syscalls](./etc/syscalls_linux_x86_64.md) for x86_64; sufficient to execute most BusyBox
  commands on both architectures.
* Advanced forking capabilities via `clone()`.
* Process and thread signal delivery and raising support.
* Dynamic ELF binary loading with support for shared libraries.

### VFS & Filesystems
* Virtual File System with full async abstractions.
* Drivers:
    * Ramdisk block device implementation.
    * FAT32 filesystem driver (ro).
    * Ext2/3/4 filesystem driver (ro).
    * `devfs` driver for kernel character device access.
    * `tmpfs` driver for temporary file storage in RAM (rw).
    * `procfs` driver for process and kernel information exposure.

## `libkernel` & Testing
`moss` is built on top of `libkernel`, a utility library designed to be
architecture-agnostic. This allows logic to be tested on a host machine (e.g.,
x86) before running on bare metal.

* Address Types: Strong typing for `VA` (Virtual), `PA` (Physical), and `UA`
  (User) addresses.
* Containers: `VMA` management, generic page-based ring buffer (`kbuf`), and
  waker sets.
* Sync Primitives: `spinlock`, `mutex`, `condvar`, `per_cpu`.
* Test Suite: A comprehensive suite of 230+ tests ensuring functionality across
  architectures (e.g., validating Aarch64 page table parsing logic on an x86
  host).
* Userspace Testing: A dedicated userspace test-suite to validate syscall behavior.

## Building and Running

### Prerequisites

**For x86_64:**
```bash
sudo apt install qemu-system-x86_64 grub-pc-bin dosfstools
rustup target add x86_64-unknown-none
```

**For AArch64:**
You will need QEMU for aarch64 emulation, dosfstools and mtools to create the
virtual file system.

```bash
sudo apt install qemu-system-aarch64 dosfstools mtools
```

Additionally you will need a version of the [aarch64-none-elf](https://developer.arm.com/Tools%20and%20Software/GNU%20Toolchain) toolchain installed.

#### Any X86-64 Linux OS
To install aarch64-none-elf on any os, download the correct release of `aarch64-none-elf` onto your computer, unpack it, then export the `bin` folder to path (Can be done via running

`export PATH="~/Downloads/arm-gnu-toolchain-X.X.relX-x86_64-aarch64-none-elf/bin:$PATH"`, X is the version number you downloaded onto your machine.

in your terminal.)

#### macOS
There is experimental support for macOS in the scripts/mac-experimental folder. The scripts in there are not guaranteed to work for all macOS users and has only been tested on an M4 Apple Silicon MacBook Air.

#### NixOS

Run the following command

```bash
nix shell nixpkgs#pkgsCross.aarch64-embedded.stdenv.cc nixpkgs#pkgsCross.aarch64-embedded.stdenv.cc.bintools
```

### Preparing the image

**For x86_64:**
The x86_64 implementation uses initrd (initial ramdisk) and doesn't require root privileges:
```bash
./scripts/create-image.sh
```

This creates a properly formatted ext4 filesystem image with all necessary files and directories. The script uses `debugfs` to populate the filesystem content without requiring loop devices or root access.

**For AArch64:**
First, run the following script to prepare the binaries for the image:
```bash
./scripts/build-deps.sh
```

This will download and build the necessary dependencies for the kernel and put them
into the `build` directory.

Once that is done, you can create the image using the following command:
```bash
./scripts/create-image.sh
```

This will create an image file named `moss.img` in the root directory of the
project, format it as ext4 image and create the necessary files and directories for the
kernel.

### Running via QEMU

**For x86_64:**
```bash
# Build and run
./scripts/qemu-runner-x86_64.sh

# Or manually:
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

**For AArch64:**
```bash
cargo run --release
```


### Running the Test Suite
Because `libkernel` is architecturally decoupled, you can run the logic tests on
your host machine:

```bash
# Test on host architecture
cargo test -p libkernel

# Test cross-architecture logic
cargo test -p libkernel --target x86_64-unknown-linux-gnu

# Run all tests including kernel tests
cargo test
```


#### Recently Completed
* **x86_64 Architecture Port**: Full implementation with multiboot2 support
* **Root-free Image Creation**: Uses debugfs instead of loop devices
* **Multi-architecture Support**: Unified codebase for ARM64 and x86_64
* **Cross-compilation**: Robust build system for multiple targets

### Roadmap & Status

moss is under active development. Current focus areas include:
* Basic Linux Syscall Compatibility (Testing through BusyBox).
* Networking Stack: TCP/IP implementation.
* Scheduler Improvements: Task load balancing.
* A fully read/write capable filesystem driver.
* Expanding coverage beyond the current 85 calls.

## Architecture Support

mtIX now supports **multiple architectures** with a unified kernel codebase:

### x86_64 Implementation (New)
Complete x86_64 port with modern features:
- **Boot**: Multiboot2 compliance with GRUB support
- **Memory**: 4-level page tables, kernel direct mapping
- **Interrupts**: Full IDT with exception handling
- **Drivers**: VirtIO, PCI, Serial, VGA console support
- **Filesystems**: Ext4 support with initrd loading
- **No Root Required**: Uses `debugfs` for image creation

For detailed documentation:
- [x86_64 Architecture](./etc/x86_64_architecture.md)
- [Dual Architecture Support](./etc/dual_architecture_support.md)

### AArch64 Implementation (Original)
Production-ready ARM64 implementation:
- **Boot**: Device tree parsing with UEFI support  
- **Memory**: 3-level page tables with CoW
- **Interrupts**: GIC interrupt controller
- **Drivers**: PL011 UART, device tree enumeration
- **SMP**: Multi-core support with IPIs

For syscall reference, see [etc/syscalls_linux_aarch64.md](./etc/syscalls_linux_aarch64.md)

### Cross-Platform Development
The kernel uses `libkernel` for architecture abstraction:
- Common code in `libkernel/src/`
- Architecture-specific code in `src/arch/*/` and `libkernel/src/arch/*/`
- Unified build system supporting multiple targets
- Cross-compilation supported via Rust targets

## Contributing

Contributions are welcome! Areas of interest:
- **New Architectures**: RISC-V, MIPS, PowerPC ports
- **Driver Development**: Network, storage, graphics drivers  
- **Filesystem Support**: FUSE, NFS, distributed filesystems
- **Performance**: SMP scalability, memory optimization
- **Testing**: Cross-architecture test coverage

## License
Distributed under the MIT License. See LICENSE for more information.
