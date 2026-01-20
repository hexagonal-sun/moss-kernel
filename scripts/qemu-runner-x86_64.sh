
#!/usr/bin/env bash
set -euo pipefail

# Always run from the project root
base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"
cd "$base"

# Create a fresh image only if it doesn't already exist
if [ ! -f moss.img ]; then
	echo "moss.img not found — creating image (requires sudo)..."
	./scripts/create-image.sh
else
	echo "Using existing moss.img"
fi

# Allow passing extra QEMU arguments
EXTRA_ARGS="${@:-}"

# QEMU command
QEMU_CMD=(qemu-system-x86_64 \
	-kernel target/x86_64-unknown-none/debug/moss32 \
	-initrd moss.img \
	-m 512M \
	-serial stdio \
	-display none \
	-no-reboot \
	-no-shutdown \
	-append "--init /bin/usertest --rootfs ext4fs --automount /dev,devfs")

if [ -n "$EXTRA_ARGS" ]; then
	QEMU_CMD+=( $EXTRA_ARGS )
fi

echo "Running: ${QEMU_CMD[*]}"
"${QEMU_CMD[@]}"
