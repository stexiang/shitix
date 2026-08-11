#!/bin/bash
# Master LFS build script for shitix kernel
# Usage: ./build-lfs-full.sh [--no-docker] [--image-size MB]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
IMG="${ROOT_DIR}/target/boot/lfs-full.img"
DOCKER_TAG="shitix-lfs-full"
IMG_SIZE_MB="${1:-512}"
[[ "$IMG_SIZE_MB" =~ ^[0-9]+$ ]] || IMG_SIZE_MB=512

cd "$SCRIPT_DIR"

echo "========================================="
echo "  shitix Full LFS Build"
echo "  Image size: ${IMG_SIZE_MB}MB"
echo "========================================="

# Step 1: Build Docker image
echo ""
echo "=== Step 1: Building LFS Docker image ==="
echo "This will download and compile all LFS packages from source."
echo "Estimated time: 2-4 hours depending on CPU"
echo ""

if ! docker image inspect "$DOCKER_TAG" &>/dev/null; then
    docker build -f Dockerfile.full -t "$DOCKER_TAG" . 2>&1 | \
        grep -E "Step|==>|ERROR|error|complete|Download" || true
else
    echo "Docker image $DOCKER_TAG already exists, skipping build."
    echo "Use --rebuild to force rebuild."
    if [[ "${2:-}" == "--rebuild" ]]; then
        docker build --no-cache -f Dockerfile.full -t "$DOCKER_TAG" .
    fi
fi

# Step 2: Extract rootfs
echo ""
echo "=== Step 2: Extracting rootfs ==="
CONTAINER=$(docker create "$DOCKER_TAG" /bin/true)
TMPDIR=$(mktemp -d)
docker export "$CONTAINER" | tar -C "$TMPDIR" -xf -
docker rm "$CONTAINER" >/dev/null

ROOTFS_SIZE=$(du -sm "$TMPDIR" | awk '{print $1}')
echo "Rootfs size: ${ROOTFS_SIZE}MB"
if [ $ROOTFS_SIZE -gt $IMG_SIZE_MB ]; then
    IMG_SIZE_MB=$((ROOTFS_SIZE + 64))
    echo "Increasing image to ${IMG_SIZE_MB}MB"
fi

# Step 3: Create ext2 image
echo ""
echo "=== Step 3: Creating ${IMG_SIZE_MB}MB ext2 image ==="
mkdir -p "$(dirname "$IMG")"
dd if=/dev/zero of="$IMG" bs=1M count=$IMG_SIZE_MB status=none
mkfs.ext2 -q -F -b 4096 "$IMG"

# Step 4: Populate
echo "=== Step 4: Populating image ==="
MNTDIR=$(mktemp -d)
mount -o loop "$IMG" "$MNTDIR"
cp -a "$TMPDIR"/. "$MNTDIR"/

# Ensure device nodes exist
mkdir -p "$MNTDIR/proc" "$MNTDIR/tmp" "$MNTDIR/run" "$MNTDIR/sys" \
    "$MNTDIR/dev" 2>/dev/null || true
chmod 1777 "$MNTDIR/tmp"
mknod "$MNTDIR/dev/null" c 1 3 2>/dev/null || true
mknod "$MNTDIR/dev/zero" c 1 5 2>/dev/null || true
mknod "$MNTDIR/dev/tty" c 5 0 2>/dev/null || true
mknod "$MNTDIR/dev/console" c 5 1 2>/dev/null || true
chmod 666 "$MNTDIR/dev/null" "$MNTDIR/dev/zero" "$MNTDIR/dev/tty" 2>/dev/null || true

# Create /init as a compiled binary (kernel can't exec scripts)
cat > /tmp/init_lfs_full.c << 'CEOF'
#include <unistd.h>
#include <sys/syscall.h>
#include <sys/mount.h>
static void w(const char *s) { int n=0; while(s[n]) n++; syscall(SYS_write,1,s,n); }
void _start(void) {
    w("shitix Full LFS 1.0\n");
    mount("proc","/proc","proc",0,0);
    mount("tmpfs","/tmp","tmpfs",0,0);
    w("Filesystems mounted.\n");
    w("Starting bash...\n");
    execl("/bin/bash","/bin/bash","--login",NULL);
    w("Bash failed, trying /bin/sh...\n");
    execl("/bin/sh","/bin/sh",NULL);
    w("Shell failed.\n");
    syscall(SYS_exit,1);
}
CEOF
docker run --rm -v /tmp:/tmp alpine:3.19 sh -c \
    'apk add --no-cache musl-dev gcc >/dev/null 2>&1 && gcc -static -nostartfiles -o /tmp/init_lfs_full /tmp/init_lfs_full.c && echo "init compiled"'

cp /tmp/init_lfs_full "$MNTDIR/init"
chmod +x "$MNTDIR/init"

umount "$MNTDIR"
rmdir "$MNTDIR"
rm -rf "$TMPDIR"

echo ""
echo "========================================="
echo "  LFS image built: $IMG"
echo "  Size: $(du -h "$IMG" | cut -f1)"
echo ""
echo "  Boot with:"
echo "    ROOTIMG=target/boot/lfs-full.img scripts/test.sh --features extra-drivers --release"
echo "========================================="
