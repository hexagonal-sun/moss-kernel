# BusyBox x86_64 Support

## Overview

BusyBox provides many standard Unix utilities in a single small executable. The moss kernel project has added x86_64 support for BusyBox, enabling cross-compilation for both aarch64 and x86_64 architectures.

## Architecture Support

### x86_64 (New)

- **Binary:** `busybox-x86_64-linux-gnu`
- **Source:** https://github.com/shutingrz/busybox-static-binaries-fat
- **Type:** Statically linked, stripped ELF 64-bit LSB executable
- **Size:** ~2.5 MB
- **Features:** Full BusyBox command set including shell, file utilities, networking, etc.

### aarch64 (Original)

- **Binary:** `busybox-aarch64-linux-gnu`
- **Source:** https://github.com/shutingrz/busybox-static-binaries-fat
- **Type:** Statically linked ELF 64-bit LSB executable
- **Features:** Full BusyBox command set

## Building BusyBox for x86_64

### Automatic Build

BusyBox is built automatically when running `build-deps.sh` with `TARGET_ARCH=x86_64`:

```bash
TARGET_ARCH=x86_64 ./scripts/build-deps.sh
```

This will:
1. Download the musl x86_64 cross-compilation toolchain (if not already present)
2. Download `busybox-x86_64-linux-gnu` from GitHub
3. Make it executable
4. Copy it to `build/bin/busybox`

### Manual Build

To build BusyBox manually for x86_64:

```bash
cd build
wget https://github.com/shutingrz/busybox-static-binaries-fat/raw/refs/heads/main/busybox-x86_64-linux-gnu
chmod +x busybox-x86_64-linux-gnu
mv busybox-x86_64-linux-gnu bin/busybox
cd ..
```

## Verification

Check the architecture of the built BusyBox binary:

```bash
$ file build/bin/busybox
build/bin/busybox: ELF 64-bit LSB executable, x86-64, version 1 (GNU/Linux), statically linked, BuildID[sha1]=2dd886577a3eb4909947b48e47d618f3ab681bb1, for GNU/Linux 3.2.0, stripped
```

## Using BusyBox in the Kernel

### Creating the Filesystem Image

After building BusyBox, create the filesystem image:

```bash
./scripts/create-image.sh
```

This will copy `busybox` to the filesystem image along with other binaries.

### Symlinks

BusyBox uses symlinks to provide multiple commands. The build system creates these symlinks based on `scripts/symlinks.cmds`. Common BusyBox commands that will be available:

- `ls`, `cp`, `mv`, `rm`, `mkdir`, `rmdir`
- `cat`, `less`, `head`, `tail`, `grep`
- `sh`, `echo`, `printf`, `test`
- `ps`, `top`, `kill`, `killall`
- `mount`, `umount`, `df`, `du`
- `ifconfig`, `ping`, `wget`, `nc`
- `tar`, `gzip`, `gunzip`, `bunzip2`
- And many more...

### Running with QEMU (x86_64)

Run the kernel with BusyBox in QEMU:

```bash
./scripts/qemu-runner-x86_64.sh
```

This will launch QEMU with the x86_64 kernel and the BusyBox-enabled filesystem.

## Architecture Comparison

| Feature | x86_64 | aarch64 |
|---------|--------|---------|
| Binary Type | ELF 64-bit LSB | ELF 64-bit LSB |
| Source | Pre-built static binary | Pre-built static binary |
| Size | ~2.5 MB | ~2.3 MB |
| Static Linking | Yes | Yes |
| Stripped | Yes | Yes |
| Build System | Automatic via build-deps.sh | Automatic via build-deps.sh |

## BusyBox Commands Available

The BusyBox binary provides approximately 350+ commands. Here are some commonly used ones:

### File Management
- `ls` - List directory contents
- `cp` - Copy files
- `mv` - Move/rename files
- `rm` - Remove files
- `mkdir` - Create directories
- `rmdir` - Remove empty directories
- `chmod` - Change file permissions
- `chown` - Change file owner

### Text Processing
- `cat` - Concatenate and display files
- `grep` - Search for patterns
- `sed` - Stream editor
- `awk` - Pattern scanning and processing
- `head` - Output first part of files
- `tail` - Output last part of files

### Shell
- `sh` - Shell command interpreter
- `echo` - Display text
- `test` - Check file types and compare values

### System
- `ps` - Report process status
- `top` - Display Linux processes
- `kill` - Terminate a process
- `killall` - Kill processes by name
- `df` - Report file system disk space usage
- `du` - Estimate file space usage

### Network
- `ping` - Send ICMP ECHO_REQUEST packets
- `ifconfig` - Configure a network interface
- `wget` - Download files from web
- `nc` - Netcat utility

### Archives
- `tar` - Tape archiver
- `gzip` - Compress files
- `gunzip` - Decompress files

## Integration with Moss Kernel

BusyBox is integrated with the moss kernel's ELF loader and syscall implementation. The kernel's syscall compatibility layer allows BusyBox commands to execute properly.

**Key syscalls used by BusyBox:**
- `open`, `close`, `read`, `write` - File I/O
- `stat`, `fstat` - File status
- `getdents` - Directory reading
- `execve` - Execute programs
- `fork`, `clone` - Process creation
- `waitpid` - Wait for process state change
- `brk`, `mmap` - Memory management
- `ioctl` - Device control

For a complete list of supported syscalls, see:
- `etc/syscalls_linux_x86_64.md`

## Troubleshooting

### Binary Not Found

If `build/bin/busybox` is missing:

```bash
# Check if build completed successfully
TARGET_ARCH=x86_64 ./scripts/deps/busybox

# Verify the file exists
ls -l build/bin/busybox
```

### Wrong Architecture

If the binary is for the wrong architecture:

```bash
# Check architecture
file build/bin/busybox

# Rebuild for correct architecture
rm build/bin/busybox
TARGET_ARCH=x86_64 ./scripts/deps/busybox
```

### BusyBox Commands Not Working

If BusyBox symlinks are missing:

1. Check `scripts/symlinks.cmds` for busybox entries
2. Ensure busybox is in `build/bin/`
3. Recreate the filesystem image: `./scripts/create-image.sh`

## Future Improvements

- Build BusyBox from source instead of using pre-built binaries
- Add BusyBox configuration options (minimal vs full-featured)
- Support for additional architectures (RISC-V, etc.)
- Integration with dynamic linking for shared libraries
- BusyBox applet selection for smaller footprint

## References

- BusyBox Documentation: https://busybox.net/BusyBox.html
- GitHub Repository: https://github.com/shutingrz/busybox-static-binaries-fat
- Linux System Calls: `etc/syscalls_linux_x86_64.md`
- Build System Documentation: `etc/docs/build-system.md`