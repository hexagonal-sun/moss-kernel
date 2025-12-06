#!/usr/bin/env bash
set -euo pipefail

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"
pushd "$base" &>/dev/null || exit 1

img="$base/moss.img"

dd if=/dev/zero of="$img" bs=1M count=128
mkfs.vfat -F 32 "$img"

mmd -i "$img" ::/bin
mmd -i "$img" ::/dev

mcopy -i "$img" "$base/build/bin"/* "::/bin"

popd &>/dev/null || exit 1
