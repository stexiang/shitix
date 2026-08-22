#!/bin/bash
# Build a glibc GNU rootfs (bash + GNU coreutils) for shitix, then wrap it into
# a bootable combined image. Replaces the musl busybox lfs3.img.
#
# Why Ubuntu base instead of LFS-from-source: the LFS 12.2 glibc-2.40 build
# is incompatible with Ubuntu's hardened GCC 13 (syslog always_inline, BZ 31928)
# and kept failing; a base Ubuntu rootfs gives the same glibc + GNU userland
# reliably. The kernel still runs it as an "actual GNU system" (bash/coreutils).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
IMG="$ROOT_DIR/lfs/gnu-full.img"
COMBINED="$ROOT_DIR/target/boot/shitix-lfs-gnu.img"

echo "=== Step 1: build Ubuntu glibc base image ==="
cat > /tmp/Dockerfile.gnu << 'EOF'
FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash coreutils util-linux mount procps findutils grep sed gawk diffutils \
    && rm -rf /var/lib/apt/lists/*
EOF
docker build -t shitix-gnu -f /tmp/Dockerfile.gnu "$SCRIPT_DIR" 2>&1 | tail -5

echo "=== Step 2: export rootfs ==="
CID=$(docker create shitix-gnu /bin/true)
TMP=$(mktemp -d)
docker export "$CID" | tar -C "$TMP" -xf -
docker rm "$CID" >/dev/null

echo "=== Step 3: install ELF init ==="
INIT_ELF="$SCRIPT_DIR/init"
[ -f "$INIT_ELF" ] || { echo "ERROR: $INIT_ELF not found"; exit 1; }
cp "$INIT_ELF" "$TMP/init"
chmod 755 "$TMP/init"
mkdir -p "$TMP/sbin"
cp "$INIT_ELF" "$TMP/sbin/init"
chmod 755 "$TMP/sbin/init"
# /bin/sh -> bash (kernel execs /bin/sh as fallback; make it bash)
ln -sf bash "$TMP/bin/sh" 2>/dev/null || true
# basic passwd so bash getpwuid works
grep -q '^root:' "$TMP/etc/passwd" 2>/dev/null || echo 'root:x:0:0:root:/root:/bin/bash' >> "$TMP/etc/passwd"

echo "=== Step 4: create ext4 image + device nodes ==="
rm -f "$IMG"
dd if=/dev/zero of="$IMG" bs=1M count=512 status=none
# 关键：禁用 extent 与 dir_index。内核 ext4 **写路径**只实现经典直接/间接
# 块指针与线性目录项；Ubuntu 默认 mkfs.ext4 开 extent（i_block 存 extent 树），
# 写路径 write_inode 把扁平化的 i.data[] 当经典指针写回会破坏 extent 头 →
# 新建文件/目录不落盘、inode 元数据全是垃圾。^extent + ^dir_index 让镜像与
# 内核已实现（e2fsck 干净的）经典布局对齐。
mkfs.ext4 -F -O ^64bit,^huge_file,^metadata_csum,^extent,^dir_index -L gnu-root "$IMG"
MNT=$(mktemp -d)
sudo mount -o loop "$IMG" "$MNT"
sudo cp -a "$TMP"/. "$MNT"/
sudo mknod -m 666 "$MNT/dev/null"    c 1 3
sudo mknod -m 666 "$MNT/dev/zero"    c 1 5
sudo mknod -m 666 "$MNT/dev/full"    c 1 7
sudo mknod -m 666 "$MNT/dev/random"  c 1 8
sudo mknod -m 666 "$MNT/dev/urandom" c 1 9
sudo mknod -m 666 "$MNT/dev/tty"     c 5 0
sudo mknod -m 622 "$MNT/dev/console" c 5 1
sudo mkdir -p "$MNT/dev/pts" "$MNT/dev/shm"
sudo chmod 1777 "$MNT/tmp"
sudo umount "$MNT"
rmdir "$MNT"
rm -rf "$TMP"

echo "=== Step 5: combined bootable image ==="
KERNEL_IMG="$ROOT_DIR/target/boot/shitix.img"
[ -f "$KERNEL_IMG" ] || { echo "ERROR: $KERNEL_IMG not found"; exit 1; }
cp "$KERNEL_IMG" "$COMBINED"
truncate -s 1M "$COMBINED"
cat "$IMG" >> "$COMBINED"

echo ""
echo "Done:"
echo "  rootfs:     $IMG ($(du -h "$IMG" | cut -f1))"
echo "  combined:   $COMBINED ($(du -h "$COMBINED" | cut -f1))"
echo ""
echo "Boot (single drive):"
echo "  qemu-system-x86_64 -drive format=raw,file=$COMBINED,if=ide -m 512M -no-reboot -no-shutdown -nographic"
