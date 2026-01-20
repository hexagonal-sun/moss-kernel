#!/usr/bin/env bash
set -euo pipefail

base="$( cd "$( dirname "${BASH_SOURCE[0]}" )"/.. && pwd )"
pushd "$base" &>/dev/null || exit 1

img="$base/moss.img"
fs_img="$base/filesystem.img"
boot_img="$base/boot.img"

# Clean up any existing files
rm -f "$img"

# Calculate sizes (in MiB)
disk_size=128

echo "Creating filesystem content in temporary directory..."
# Create filesystem content in a temporary directory
mntd=$(mktemp -d)

# Create standard layout and copy build artifacts
mkdir -p "$mntd"/bin "$mntd"/dev "$mntd"/proc "$mntd"/tmp "$mntd"/boot/grub
for file in "$base/build/bin"/*; do
    if [ -f "$file" ]; then
        cp "$file" "$mntd/bin/$(basename "$file")"
    fi
done

# Create symlinks listed in scripts/symlinks.cmds (supports simple ln -s and mkdir lines)
if [ -f "$base/scripts/symlinks.cmds" ]; then
    while IFS= read -r line; do
        # skip empty/comment lines
        case "$line" in
            ""|\#*) continue ;;
        esac
        # support commands like: mkdir /bin or symlink /bin/[[ /bin/busybox
        set -- $line
        cmd=$1; shift
        if [ "$cmd" = "mkdir" ]; then
            dst="$mntd$1"
            mkdir -p "$dst"
        elif [ "$cmd" = "symlink" ]; then
            src="$1"; dst="$2"
            dstpath="$mntd$dst"
            srcpath="$mntd$src"
            # ensure target dir exists
            mkdir -p "$(dirname "$dstpath")"
            # Check if the source file exists before creating symlink
            if [ -e "$srcpath" ]; then
                ln -s "${src#*/bin/}" "$dstpath" || ln -s "$src" "$dstpath" || true
            else
                # Skip symlink if source doesn't exist (e.g., busybox not built)
                true
            fi
        fi
    done < "$base/scripts/symlinks.cmds"
fi

# Ensure /boot exists for GRUB
mkdir -p "$mntd/boot/grub"

# Write minimal grub.cfg to boot kernel ELF
cat > "$mntd/boot/grub/grub.cfg" <<EOF
set timeout=1
set default=0
menuentry "moss kernel" {
    multiboot /bin/kernel.elf --init /bin/usertest --rootfs ext4fs --automount /dev,devfs
    module /moss.img
}
EOF

echo "Creating ext4 filesystem for initrd..."
# Create a temporary ext4 filesystem image
temp_fs="$base/temp_ext4.img"
dd if=/dev/zero of="$temp_fs" bs=1M count=32  # 32MB should be enough
mkfs.ext4 -F "$temp_fs"

# Try to copy content using debugfs (no mount required)
echo "Attempting to copy content to ext4 filesystem..."
cd "$mntd"

# Use debugfs with proper syntax
debugfs -w -R "mkdir /bin" "$temp_fs" >/dev/null 2>&1 || true
debugfs -w -R "mkdir /dev" "$temp_fs" >/dev/null 2>&1 || true
debugfs -w -R "mkdir /proc" "$temp_fs" >/dev/null 2>&1 || true
debugfs -w -R "mkdir /tmp" "$temp_fs" >/dev/null 2>&1 || true
debugfs -w -R "mkdir /boot" "$temp_fs" >/dev/null 2>&1 || true
debugfs -w -R "mkdir /boot/grub" "$temp_fs" >/dev/null 2>&1 || true

# Copy files
for file in bin/*; do
    if [ -f "$file" ]; then
        debugfs -w -R "write $file /$file" "$temp_fs" >/dev/null 2>&1 || true
    fi
done

# Write GRUB config
cat > grub.cfg <<EOF
set timeout=1
set default=0
menuentry "moss kernel" {
    multiboot /bin/kernel.elf --init /bin/usertest --rootfs ext4fs --automount /dev,devfs
    module /moss.img
}
EOF
debugfs -w -R "write grub.cfg /boot/grub/grub.cfg" "$temp_fs" >/dev/null 2>&1 || true
rm -f grub.cfg

cd "$base"
echo "Content copied using debugfs."

# Move the ext4 filesystem to final location and clean up
mv "$temp_fs" "$img"

# Pad to page boundary (4096 bytes) for RamdiskBlkDev  
current_size=$(stat -c%s "$img")
page_size=4096
padded_size=$(( ((current_size + page_size - 1) / page_size) * page_size ))
if [ $current_size -ne $padded_size ]; then
    echo "Padding initrd from $current_size to $padded_size bytes (page boundary)"
    dd if=/dev/zero bs=1 count=$((padded_size - current_size)) >> "$img"
fi

# Clean up the temporary directory
rm -rf "$mntd"

echo "Raw filesystem image created successfully: $img"
echo "Size: $(stat -c%s "$img") bytes"

popd &>/dev/null || exit 1
