#!/usr/bin/env bash
set -euo pipefail

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"
img="$base/moss.img"

echo "Creating raw 128MB EXT4 image at $img..."
rm -f "$img"
dd if=/dev/zero of="$img" bs=1M count=128
mkfs.ext4 -F "$img"

echo "Populating image..."
debugfs -w -R "mkdir /bin" "$img"
debugfs -w -R "write $base/build/bin/usertest /bin/usertest" "$img"
debugfs -w -R "write $base/build/bin/kernel.elf /bin/kernel.elf" "$img"

# Add some dummy directories
debugfs -w -R "mkdir /dev" "$img"
debugfs -w -R "mkdir /root" "$img"
debugfs -w -R "mkdir /tmp" "$img"

echo "Done."
