#!/usr/bin/env bash
set -e

# First argument: the ELF file to run (required)
# Second argument: init script to run (optional, defaults to /bin/bash)
# Parse args:
if [ $# -lt 1 ]; then
    echo "Usage: $0 <elf-file>"
    exit 1
fi

if [ -n "$2" ]; then
    append_args="--init=$2"
else
    append_args="--init=/bin/bash --init-arg=-i"
fi


base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"

elf="$1"
bin="${elf%.elf}.bin"

# Convert to binary format
aarch64-none-elf-objcopy -O binary "$elf" "$bin"
qemu-system-aarch64 -M virt,gic-version=3 -initrd moss.img -cpu cortex-a72 -m 2G -smp 4 -nographic -s -kernel "$bin" -append "$append_args --rootfs=ext4fs --automount=/dev,devfs --automount=/tmp,tmpfs --automount=/proc,procfs"
