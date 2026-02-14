# moss

![Architecture](https://img.shields.io/badge/arch-aarch64-blue)
![Language](https://img.shields.io/badge/language-Rust-orange)
![License](https://img.shields.io/badge/license-MIT-yellow)
[![IRC](https://img.shields.io/badge/OFTC_IRC-%23moss-blue)](https://webchat.oftc.net/?nick=&channels=%23moss)

![Moss Boot Demo](etc/moss_demo.gif)

**moss** is a Unix-like, Linux-compatible kernel written in Rust and AArch64
assembly.

It features an asynchronous kernel core, a modular architecture abstraction
layer, and binary compatibility with Linux userspace applications. Moss is
currently capable of running a dynamically linked Arch Linux AArch64 userspace,
including bash, BusyBox, coreutils, ps, top, and strace.

## Features

### Architecture & Memory
* Full support for AArch64.
* A well-defined HAL allowing for easy porting to other architectures (e.g.,
  x86_64, RISC-V).
* Memory Management:
    * Full MMU enablement and page table management.
    * Copy-on-Write (CoW) pages.
    * Safe copy to/from userspace async functions.
    * Kernel and userspace page fault management.
    * Kernel stack-overflow detection.
    * Shared library mapping and relocation.
    * `/proc/self/maps` support.
    * Buddy allocator for physical addresses and `smalloc` for boot allocations
      and tracking memory reservations.
    * A full slab allocator for kernel object allocations, featureing a per-CPU
      object cache.

### Async Core
One of the defining features of `moss` is its usage of Rust's `async/await`
model within the kernel context:
* All non-trivial system calls are written as `async` functions, sleep-able
  functions are prefixed with `.await`.
* The compiler enforces that spinlocks cannot be held over sleep points,
  eliminating a common class of kernel deadlocks.
* Any future can be wrapped with the `.interruptable()` combinator, allowing
  signals to interrupt the waiting future and appropriate action to be taken.

### Process Management
* Full task management including both UP and SMP scheduling via EEVDF and task
  migration via IPIs.
* Capable of running dynamically linked ELF binaries from Arch Linux.
* Currently implements [105 Linux syscalls](./etc/syscalls_linux_aarch64.md)
* `fork()`, `execve()`, `clone()`, and full process lifecycle management.
* Job control support (process groups, waitpid, background tasks).
* Signal delivery, masking, and propagation (SIGTERM, SIGSTOP, SIGCONT, SIGCHLD,
  etc.).
* ptrace support sufficient to run strace on Arch binaries.

### VFS & Filesystems
* Virtual File System with full async abstractions.
* Drivers:
    * Ramdisk block device implementation.
    * FAT32 filesystem driver (ro).
    * Ext2/3/4 filesystem driver (read support, partial write support).
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
  architectures (e.g., validating AArch64 page table parsing logic on an x86
  host).
* Userspace Testing, `usertest`: A dedicated userspace test-suite to validate
  syscall behavior in the kernel at run-time.

## Building and Running

### Prerequisites
You will need QEMU for AArch64 emulation, as well as dosfstools and mtools to create the
virtual file system.

```bash
sudo apt install qemu-system-aarch64 dosfstools mtools
```

Additionally you will need a version of the [aarch64-none-elf](https://developer.arm.com/Tools%20and%20Software/GNU%20Toolchain) toolchain installed.

#### Any x86_64 Linux OS
To install `aarch64-none-elf` on any OS, download the appropriate release of `aarch64-none-elf` onto your computer, unpack it, then export the `bin` directory to PATH (Can be done via running:

`export PATH="~/Downloads/arm-gnu-toolchain-X.X.relX-x86_64-aarch64-none-elf/bin:$PATH"`, where X is the version number you downloaded onto your machine.

in your terminal.)

#### macOS
There is experimental support for macOS in the scripts/mac-experimental folder. The scripts in there are not guaranteed to work for all macOS users and has only been tested on an M4 Apple Silicon MacBook Air.

#### NixOS

Run the following command:

```bash
nix shell nixpkgs#pkgsCross.aarch64-embedded.stdenv.cc nixpkgs#pkgsCross.aarch64-embedded.stdenv.cc.bintools
```

### Preparing the image

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
project, format it as ext4 image and create the necessary files and directories for
the kernel.

### Running via QEMU

To build the kernel and launch it in QEMU:

``` bash
cargo run --release
```


### Running the Test Suite
Because `libkernel` is architecturally decoupled, you can run the logic tests on
your host machine:

``` bash
cargo test -p libkernel --target x86_64-unknown-linux-gnu
```


### Roadmap & Status

moss is under active development. Current focus areas include:

* Networking Stack: TCP/IP implementation.
* A fully read/write capable filesystem driver.
* Expanding coverage beyond the current 105 calls.
* systemd bringup.

## Non-Goals (for now)

* Binary compatibility beyond AArch64.
* Production hardening.

Moss is an experimental kernel focused on exploring asynchronous design and
Linux ABI compatibility in Rust.

## Contributing

Contributions are welcome! Whether you are interested in writing a driver,
porting to x86, or adding syscalls.

## License
Distributed under the MIT License. See LICENSE for more information.
