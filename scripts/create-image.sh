#!/usr/bin/env bash
set -euo pipefail

# Error if mkfs.ext4 is not installed
if ! command -v mkfs.ext4 &> /dev/null; then
    echo "mkfs.ext4 could not be found. Please install e2fsprogs."
    exit 1
fi
# Error if wget is not installed
if ! command -v wget &> /dev/null; then
    echo "wget could not be found. Please install wget."
    exit 1
fi
# Error if jq is not installed
if ! command -v jq &> /dev/null; then
    echo "jq could not be found. Please install jq."
    exit 1
fi

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"
pushd "$base" &>/dev/null || exit 1

img="$base/moss.img"

if [ -f "$img" ]; then
    rm "$img"
fi

touch "$img"
mkfs.ext4 "$img" 512M

# Download alpine minirootfs to $base/build/ if it doesn't exist
if [ ! -f "$base/build/alpine-minirootfs.tar.gz" ]; then
    echo "Downloading alpine minirootfs..."
    mkdir -p "$base/build"
    wget -O "$base/build/alpine-minirootfs.tar.gz" https://dl-cdn.alpinelinux.org/alpine/v3.23/releases/aarch64/alpine-minirootfs-3.23.3-aarch64.tar.gz
fi

# Extract to directory $base/build/rootfs
if [ -d "$base/build/rootfs" ]; then
    rm -rf "$base/build/rootfs"
fi
mkdir -p build/rootfs
tar -xzf "$base/build/alpine-minirootfs.tar.gz" -C "$base/build/rootfs"

# Copy any extra binaries in $base/build/extra_bins to $base/build/rootfs/bin
if [ -d "$base/build/extra_bins" ]; then
    cp "$base/build/extra_bins/"* "$base/build/rootfs/bin/"
fi

# Build and copy over usertest
cd "$base"/usertest
usertest_binary="$(cargo build --message-format=json | jq -r 'select(.reason == "compiler-artifact") | .filenames[]' | grep "usertest")"
cp "$usertest_binary" "$base/build/rootfs/bin/usertest"

# make image
yes | mkfs.ext4 -d "$base/build/rootfs" "$img" || true
