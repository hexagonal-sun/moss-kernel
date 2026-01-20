# Quick Start Guide - x86_64 Support

## Overview

This guide provides quick steps to build and run the moss kernel with x86_64 BusyBox support.

## Prerequisites

Ensure you have the following installed:
- QEMU: `sudo apt install qemu-system-x86`
- wget: `sudo apt install wget`
- Build tools: `sudo apt install build-essential`

## Building for x86_64

### Step 1: Build Dependencies

Build all required binaries (BusyBox, Bash, usertest) for x86_64:

```bash
TARGET_ARCH=x86_64 ./scripts/build-deps.sh
```

This will:
- Download x86_64 musl cross-compilation toolchain (~110 MB)
- Download BusyBox x86_64 binary (~2.5 MB)
- Build Bash from source for x86_64
- Build usertest for x86_64
- Place all binaries in `build/bin/`

**Expected time:** 2-5 minutes (depending on internet connection)

### Step 2: Create Filesystem Image

Create the ext4 filesystem image with all binaries:

```bash
./scripts/create-image.sh
```

This creates `moss.img` (~32 MB) containing:
- `/bin/busybox` - BusyBox with all utilities
- `/bin/bash` - Bash shell
- `/bin/usertest` - Kernel test binary
- Symlinks to BusyBox commands

### Step 3: Build and Run the Kernel

Build the x86_64 kernel and launch in QEMU:

```bash
cargo run --release
```

Or use the dedicated QEMU runner script:

```bash
./scripts/qemu-runner-x86_64.sh
```

## Verification

### Check Binaries

Verify the binaries are x86_64:

```bash
file build/bin/busybox build/bin/bash build/bin/usertest
```

Expected output:
```
build/bin/busybox: ELF 64-bit LSB executable, x86-64, ...
build/bin/bash: ELF 64-bit LSB pie executable, x86-64, ...
build/bin/usertest: ELF 64-bit LSB pie executable, x86-64, ...
```

### Check Filesystem Image

Verify the image was created:

```bash
ls -lh moss.img
file moss.img
```

### Test in QEMU

Run the kernel and verify BusyBox works:

```bash
./scripts/qemu-runner-x86_64.sh
```

In the running system, test BusyBox commands:
```bash
# Available commands
ls -l /bin/

# Test basic utilities
echo "Hello from BusyBox on x86_64!"
cat /proc/version
ps aux
df -h

# Test networking (if available)
ping -c 1 8.8.8.8
```

## Switching Between Architectures

### Build for x86_64

```bash
# Clean previous builds (optional)
rm -rf build/bin/*

# Build for x86_64
TARGET_ARCH=x86_64 ./scripts/build-deps.sh

# Create image
./scripts/create-image.sh

# Run
./scripts/qemu-runner-x86_64.sh
```

### Build for aarch64

```bash
# Clean previous builds (optional)
rm -rf build/bin/*

# Build for aarch64
TARGET_ARCH=aarch64 ./scripts/build-deps.sh

# Create image
./scripts/create-image.sh

# Run
./scripts/qemu-runner.sh
```

## Common Tasks

### Rebuild Single Component

To rebuild just BusyBox:

```bash
TARGET_ARCH=x86_64 ./scripts/deps/busybox
```

To rebuild just Bash:

```bash
TARGET_ARCH=x86_64 ./scripts/deps/bash
```

To rebuild just usertest:

```bash
TARGET_ARCH=x86_64 ./scripts/deps/usertest
```

### Clean Build

To clean and rebuild everything:

```bash
# Remove build artifacts
rm -rf build/bin/* moss.img

# Rebuild
TARGET_ARCH=x86_64 ./scripts/build-deps.sh
./scripts/create-image.sh
```

### Check Available BusyBox Commands

List all BusyBox commands:

```bash
# On host system
./build/bin/busybox --list

# In running kernel
/bin/busybox --list
```

## Troubleshooting

### Build Fails

**Problem:** `wget` fails to download toolchain or binaries

**Solution:** Check internet connection and try again:
```bash
# Check musl.cc is accessible
wget --spider https://musl.cc/x86_64-linux-musl-cross.tgz

# Retry build
TARGET_ARCH=x86_64 ./scripts/build-deps.sh
```

### Wrong Architecture

**Problem:** Binaries are wrong architecture

**Solution:** Verify TARGET_ARCH is set:
```bash
# Check current architecture
uname -m

# Set target explicitly
TARGET_ARCH=x86_64 ./scripts/build-deps.sh
```

### Image Not Found

**Problem:** `moss.img` doesn't exist

**Solution:** Create the image:
```bash
./scripts/create-image.sh
```

### QEMU Won't Start

**Problem:** QEMU fails to launch

**Solution:** Check QEMU is installed:
```bash
# Verify QEMU
qemu-system-x86_64 --version

# Install if missing
sudo apt install qemu-system-x86
```

## Next Steps

- Read [Build System Documentation](build-system.md) for details
- Read [BusyBox x86_64 Documentation](busybox-x86_64.md) for features
- Check [Syscalls](../syscalls_linux_x86_64.md) for supported Linux syscalls
- Explore kernel source code in `../src/`

## Performance Tips

### Faster Rebuilds

Skip toolchain download if already present:
```bash
# Toolchain already exists in build/
# Only rebuild binaries
TARGET_ARCH=x86_64 ./scripts/build-deps.sh
```

### Parallel Building

The build system supports parallel execution of dependency scripts.

### Caching

Toolchains and source tarballs are cached in `build/` directory. Only binaries in `build/bin/` are cleaned.

## Getting Help

- Check documentation in `etc/docs/`
- Review README.md in project root
- Check build logs for errors
- Verify architecture with `uname -m` and `file build/bin/*`

## Environment Variables Summary

| Variable | Default | Purpose |
|----------|---------|---------|
| `TARGET_ARCH` | `uname -m` | Target architecture for builds |
| `ARCH` | `uname -m` | Host architecture |
| `CC` | Auto-set | C compiler for cross-compilation |
| `stdlib` | `musl` | C library type |

## File Sizes

| Component | Size |
|-----------|------|
| BusyBox x86_64 | ~2.5 MB |
| Bash x86_64 | ~4.4 MB |
| usertest x86_64 | ~6.3 MB |
| moss.img | ~32 MB |
| musl toolchain | ~110 MB (one-time download) |

## Architecture Comparison

| Aspect | x86_64 | aarch64 |
|--------|--------|---------|
| QEMU System | `qemu-system-x86_64` | `qemu-system-aarch64` |
| QEMU Runner | `scripts/qemu-runner-x86_64.sh` | `scripts/qemu-runner.sh` |
| BusyBox Binary | `busybox-x86_64-linux-gnu` | `busybox-aarch64-linux-gnu` |
| Toolchain | `x86_64-linux-musl-cross` | `aarch64-linux-musl-cross` |