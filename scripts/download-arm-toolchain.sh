#!/bin/bash
# First and only argument: where to download the toolchain file

if [ $# -ne 1 ]; then
    echo "Usage: $0 <download-path>"
    exit 1
fi

# Get os and arch
OS="$(uname | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

# Reject darwin
if [ "$OS" == "darwin" ]; then
    echo "Error: macOS is not supported."
    exit 1
fi

# Substitute ARCH arm64 for aarch64
if [ "$ARCH" == "arm64" ]; then
    ARCH="aarch64"
fi
echo "Detected OS: $OS"
echo "Detected ARCH: $ARCH"


# Example: https://developer.arm.com/-/media/Files/downloads/gnu/15.2.rel1/binrel/arm-gnu-toolchain-15.2.rel1-x86_64-aarch64-none-elf.tar.xz
DOWNLOAD_URL="https://developer.arm.com/-/media/Files/downloads/gnu/15.2.rel1/binrel/arm-gnu-toolchain-15.2.rel1-${ARCH}-aarch64-none-elf.tar.xz"
wget -O "$1" "$DOWNLOAD_URL"
