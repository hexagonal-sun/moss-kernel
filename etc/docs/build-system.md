# Build System Architecture

## Overview

The moss kernel uses a flexible build system that supports multiple architectures, including aarch64 and x86_64. The build system is designed to:
- Download and compile cross-compilation toolchains
- Build userspace binaries (busybox, bash) for the target architecture
- Create filesystem images for use with QEMU

## Architecture Detection

The build system automatically detects the host architecture via `uname -m`, but also supports explicit target architecture specification through environment variables.

### Environment Variables

- `ARCH`: Host architecture (auto-detected via `uname -m`)
- `TARGET_ARCH`: Target architecture for cross-compilation
- `CC`: C compiler (automatically set by build scripts)
- `stdlib`: C library type (defaults to `musl`)

## Build Scripts

### `scripts/build-deps.sh`

Main build script that:
1. Detects architecture and downloads appropriate musl toolchain
2. Executes all build scripts in `scripts/deps/`
3. Places built binaries in `build/bin/`

**Usage:**
```bash
# Build for current architecture
./scripts/build-deps.sh

# Build for x86_64 (cross-compilation)
TARGET_ARCH=x86_64 ./scripts/build-deps.sh

# Build for aarch64
TARGET_ARCH=aarch64 ./scripts/build-deps.sh
```

### `scripts/deps/busybox`

Downloads pre-built static busybox binaries for the target architecture from:
- `https://github.com/shutingrz/busybox-static-binaries-fat`

**Supported architectures:**
- `aarch64`: `busybox-aarch64-linux-gnu`
- `x86_64`: `busybox-x86_64-linux-gnu`

### `scripts/deps/bash`

Builds bash from source for the target architecture:
- Downloads bash 5.3 from GNU FTP
- Configures for static linking with musl
- Compiles and places in `build/bin/bash`

**Build process:**
1. Download bash-5.3 source
2. Configure with: `--without-bash-malloc --enable-static-link`
3. Build using cross-compilation toolchain
4. Output to `build/bin/bash`

### `scripts/create-image.sh`

Creates the filesystem image (`moss.img`) used by QEMU:
1. Creates directory structure: `/bin`, `/dev`, `/proc`, `/tmp`, `/boot/grub`
2. Copies binaries from `build/bin/` to filesystem
3. Creates symlinks from `scripts/symlinks.cmds`
4. Creates GRUB configuration for multiboot
5. Creates ext4 filesystem image
6. Pads to page boundary (4096 bytes)

**Usage:**
```bash
./scripts/create-image.sh
```

## Toolchain Management

### Musl Cross-Compilation Toolchains

The build system downloads musl cross-compilation toolchains from:
- Primary: `https://musl.cc/`
- Fallback: `https://github.com/arihant2math/prebuilt-musl/`

**Downloaded toolchains:**
- `aarch64-linux-musl-cross` (for building aarch64 binaries on x86_64 hosts)
- `x86_64-linux-musl-cross` (for building x86_64 binaries on aarch64 hosts)

**Native toolchains:**
- `aarch64-linux-musl-native` (when building on aarch64 hosts)

## Architecture-Specific Details

### x86_64

**Toolchain:** `x86_64-linux-musl-cross`
- Location: `build/x86_64-linux-musl-cross/`
- Compiler: `build/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc`

**Binaries:**
- Busybox: `busybox-x86_64-linux-gnu` (statically linked)
- Bash: Built from source with `--host=x86_64-linux-musl`

**QEMU runner:** `scripts/qemu-runner-x86_64.sh`

### aarch64

**Toolchain:** `aarch64-linux-musl-cross` (or `aarch64-linux-musl-native`)
- Location: `build/aarch64-linux-musl-cross/` (or `build/aarch64-linux-musl-native/`)
- Compiler: `build/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc`

**Binaries:**
- Busybox: `busybox-aarch64-linux-gnu` (statically linked)
- Bash: Built from source with `--host=aarch64-linux-musl`

**QEMU runner:** `scripts/qemu-runner.sh`

## Output Structure

```
build/
├── bin/                    # Compiled binaries for target architecture
│   ├── busybox             # Busybox executable
│   └── bash                # Bash executable
├── <arch>-linux-musl-cross/   # Cross-compilation toolchain
│   └── bin/
│       └── <arch>-linux-musl-gcc
└── <arch>-linux-musl*.tgz     # Downloaded toolchain archive

moss.img                   # Filesystem image for initrd
```

## Troubleshooting

### Build failures

If build-deps.sh fails:
1. Check if musl.cc is accessible
2. Verify wget is installed
3. Try cleaning `build/` directory and rebuilding

### Architecture mismatch

If binaries are wrong architecture:
```bash
# Check binary architecture
file build/bin/busybox

# Rebuild for correct architecture
TARGET_ARCH=x86_64 ./scripts/build-deps.sh
```

### Toolchain issues

If compiler is not found:
1. Verify toolchain was downloaded in `build/`
2. Check that CC environment variable is set correctly
3. Ensure `build/<arch>-linux-musl-cross/bin/` exists

## Future Enhancements

- Support for RISC-V architecture
- Build from source for busybox instead of pre-built binaries
- Support for alternative C libraries (glibc, uclibc)
- Parallel build support
- Caching of build artifacts