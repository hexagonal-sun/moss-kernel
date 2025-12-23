#!/usr/bin/env bash
set -euo pipefail

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"

elf="$1"
bin="${elf%.elf}.bin"

# Locate a suitable objcopy implementation (prefer bare-metal, fall back to linux/llvm/rust versions)
OBJCOPY=${OBJCOPY:-$(command -v aarch64-none-elf-objcopy \
                  || command -v aarch64-linux-gnu-objcopy \
                  || command -v llvm-objcopy \
                  || command -v rust-objcopy)}

if [[ -z "$OBJCOPY" ]]; then
    echo "Error: no compatible aarch64 objcopy found (looked for aarch64-none-elf-objcopy, aarch64-linux-gnu-objcopy, llvm-objcopy, rust-objcopy)." >&2
    exit 1
fi

# Convert to binary format
"$OBJCOPY" -O binary "$elf" "$bin"
qemu-system-aarch64 -M virt,gic-version=3 -initrd moss.img -cpu cortex-a72 -m 2G -smp 4 -nographic -s -kernel "$bin" -append "--init=/usertest --rootfs=fat32fs --automount=/dev,devfs --automount=/tmp,tmpfs"
