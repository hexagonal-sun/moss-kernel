#!/usr/bin/env bash
set -euo pipefail

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"

mkdir -p "$base/build/bin"
rm -f "$base/build/bin"/*

pushd "$base/build" &>/dev/null || exit 1

if [ "$(uname -m)" == 'aarch64' ]; then
    MUSL_CC="aarch64-linux-musl-native"
else
    MUSL_CC="aarch64-linux-musl-cross"
fi

# Decide download source by checking musl.cc reachability first
PRIMARY_URL="https://musl.cc/${MUSL_CC}.tgz"
FALLBACK_URL="https://github.com/arihant2math/prebuilt-musl/raw/refs/heads/main/${MUSL_CC}.tgz"

if [ ! -f "${MUSL_CC}.tgz" ]; then
    echo "Checking musl.cc reachability..."
    if wget --spider --timeout=5 --tries=1 "$PRIMARY_URL" >/dev/null 2>&1; then
        DOWNLOAD_URL="$PRIMARY_URL"
        echo "musl.cc reachable. Using primary source."
    else
        echo "musl.cc not reachable. Using fallback source..."
        DOWNLOAD_URL="$FALLBACK_URL"
    fi

    # Perform the actual download from the chosen source
    wget "$DOWNLOAD_URL"
    tar -xzf "${MUSL_CC}.tgz"
fi

popd &>/dev/null || exit 1

build=${build:-$(ls $base/scripts/deps)}

export CC="$base/build/${MUSL_CC}/bin/aarch64-linux-musl-gcc"

for script in "$base/scripts/deps/"*
do
    [ -e "$script" ] || continue # skip if no file exists
    [ -x "$script" ] || continue # skip if not executable

    filename=$(basename "$script")

    if [[ "$filename" == _* ]]
    then
        echo "Skipping: $filename"
        continue
    fi

    # skip if not in build list
    if ! grep -qw "$filename" <<< "$build";
    then
        echo "Skipping: $filename"
        continue
    fi

    echo "Preparing: $filename"

    # make sure each script is run in the base directory
    pushd "$base" &>/dev/null || exit 1
    bash "$script"
    popd &>/dev/null || exit 1
done
