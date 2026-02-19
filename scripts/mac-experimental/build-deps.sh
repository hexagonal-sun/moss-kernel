#!/usr/bin/env bash
set -euo pipefail

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/../.. && pwd )"

mkdir -p "$base/build/bin"
rm -f "$base/build/bin"/*

pushd "$base/build" &>/dev/null || exit 1

popd &>/dev/null || exit 1

build=${build:-$(ls $base/scripts/deps)}

for script in "$base/scripts/mac-experimental/deps/"*
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
