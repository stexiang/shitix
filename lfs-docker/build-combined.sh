#!/bin/bash
# Build a single combined bootable image: kernel + LFS rootfs
# The kernel image occupies the first 1MB. The LFS rootfs (ext2) follows.
# The kernel tries both IDE drives (dev 3,0 and 3,1).
#
# For dual-drive: kernel on drive 0, rootfs on drive 1 (ROOTIMG approach)
# For combined: everything on one disk, kernel mounts dev 3,0
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
KERNEL_IMG="$ROOT_DIR/target/boot/shitix.img"
LFS_IMG="$ROOT_DIR/target/boot/lfs.img"
COMBINED="$ROOT_DIR/target/boot/shitix-lfs-combined.img"

echo "=== Building combined shitix + LFS image ==="

[ -f "$KERNEL_IMG" ] || { echo "ERROR: $KERNEL_IMG not found. Run: bash scripts/build.sh --release --features extra-drivers"; exit 1; }
[ -f "$LFS_IMG" ] || { echo "ERROR: $LFS_IMG not found. Run: bash lfs-docker/build-lfs-full.sh"; exit 1; }

KERNEL_SIZE=$(stat -c%s "$KERNEL_IMG")
# Round kernel to 1MB boundary
PAD=$(( ((KERNEL_SIZE + 1048575) / 1048576) * 1048576 ))
LFS_SIZE=$(stat -c%s "$LFS_IMG")
COMBINED_SIZE=$((PAD + LFS_SIZE))

echo "Kernel:    $KERNEL_SIZE bytes"
echo "Pad to:    $PAD bytes (1MB aligned)"
echo "LFS image: $LFS_SIZE bytes"
echo "Combined:  $COMBINED_SIZE bytes ($((COMBINED_SIZE / 1048576))MB)"

# Build combined image
cp "$KERNEL_IMG" "$COMBINED"
truncate -s "$PAD" "$COMBINED"
cat "$LFS_IMG" >> "$COMBINED"

echo ""
echo "Combined image: $COMBINED ($(du -h "$COMBINED" | cut -f1))"
echo ""
echo "=== Boot commands ==="
echo ""
echo "# Dual-drive (recommended for development):"
echo "  qemu-system-x86_64 \\"
echo "      -drive format=raw,file=$KERNEL_IMG,if=ide \\"
echo "      -drive format=raw,file=$LFS_IMG,if=ide \\"
echo "      -m 256M -no-reboot -nographic"
echo ""
echo "# Single-drive (combined image):"
echo "  qemu-system-x86_64 \\"
echo "      -drive format=raw,file=$COMBINED,if=ide \\"
echo "      -m 256M -no-reboot -nographic"
echo ""
echo "# Automated test (dual-drive):"
echo "  ROOTIMG=$LFS_IMG bash scripts/test.sh --release --features extra-drivers"
