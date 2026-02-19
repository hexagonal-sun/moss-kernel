#!/usr/bin/env bash
set -euo pipefail

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"
pushd "$base" &>/dev/null || exit 1

img="$base/moss.img"

dd if=/dev/zero of="$img" bs=1M count=128
mkfs.ext4 -F "$img"

debugfs -w -f  "$base/scripts/symlinks.cmds" "$img"
for file in "$base/build/bin"/*; do
    debugfs -w "$img" -R "write $file /bin/$(basename "$file")"
done

popd &>/dev/null || exit 1
