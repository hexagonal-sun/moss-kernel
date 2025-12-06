#!/usr/bin/env bash
set -e

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"

elf="$1"
bin="${elf%.elf}.bin"

# Convert to binary format
aarch64-none-elf-objcopy -O binary "$elf" "$bin"
qemu-system-aarch64 -M virt,gic-version=3 -initrd moss.img -cpu cortex-a72 -m 2G -smp 4 -nographic -s -kernel "$bin" -append "--init=/bin/bash --rootfs=fat32fs --automount=/dev,devfs"
