#!/usr/bin/env bash
set -e

cargo build --release

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )/target/aarch64-unknown-none-softfloat/release/"

elf="$( echo $base )moss"
bin="${elf%.elf}.bin"

# Convert to binary format
aarch64-none-elf-objcopy -O binary "$elf" "$bin"
qemu-system-aarch64 -M virt,gic-version=3 -initrd moss.img -cpu cortex-a72 -m 2G -smp 4 -nographic -s -kernel "$bin" -append "--init=/usertest --rootfs=fat32fs --automount=/dev,devfs --automount=/tmp,tmpfs" > test_output.log 2>&1
# Check for line saying "All tests passed"
if grep -q "All tests passed" output.log; then
    echo "All tests passed"
    exit 0
else
    echo "Some tests failed"
    cat output.log
    exit 1
fi
