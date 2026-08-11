#!/bin/bash
# Build LFS rootfs ext2 image for shitix from Alpine Docker image
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
IMG="$ROOT_DIR/target/boot/lfs.img"
DOCKER_TAG="shitix-lfs"
IMG_SIZE_MB=256

cd "$SCRIPT_DIR"

echo "==> Building LFS Docker image..."
docker build -t "$DOCKER_TAG" . 2>&1 | grep -E "Step|ERROR|error|DONE" || true

echo "==> Extracting rootfs from Docker image..."
CONTAINER=$(docker create "$DOCKER_TAG" true 2>/dev/null || echo "")
if [ -z "$CONTAINER" ] || [ "$CONTAINER" = "" ]; then
    echo "ERROR: Could not create container from $DOCKER_TAG"
    exit 1
fi
TMPDIR=$(mktemp -d)
docker export "$CONTAINER" | tar -C "$TMPDIR" -xf -
docker rm "$CONTAINER" >/dev/null

echo "==> Creating ${IMG_SIZE_MB}MB ext2 image..."
mkdir -p "$(dirname "$IMG")"
dd if=/dev/zero of="$IMG" bs=1M count=$IMG_SIZE_MB status=none
mkfs.ext2 -q -F -b 4096 "$IMG"

echo "==> Populating image..."
MNTDIR=$(mktemp -d)
mount -o loop "$IMG" "$MNTDIR"
cp -a "$TMPDIR"/. "$MNTDIR"/

# Create device nodes
mknod "$MNTDIR/dev/null" c 1 3 2>/dev/null || true
mknod "$MNTDIR/dev/zero" c 1 5 2>/dev/null || true
mknod "$MNTDIR/dev/tty" c 5 0 2>/dev/null || true
mknod "$MNTDIR/dev/console" c 5 1 2>/dev/null || true
chmod 666 "$MNTDIR/dev/null" "$MNTDIR/dev/zero" "$MNTDIR/dev/tty" 2>/dev/null || true
chmod 600 "$MNTDIR/dev/console" 2>/dev/null || true

mkdir -p "$MNTDIR/proc" "$MNTDIR/tmp" "$MNTDIR/run" "$MNTDIR/sys" 2>/dev/null || true
chmod 1777 "$MNTDIR/tmp" 2>/dev/null || true
chmod 1777 "$MNTDIR/var/tmp" 2>/dev/null || true

# Create /init as first process
cat > "$MNTDIR/init" << 'INIT'
#!/bin/sh
export PATH=/bin:/usr/bin:/sbin:/usr/sbin
export HOME=/root
export TERM=linux
cd /root 2>/dev/null || true
exec /bin/bash --login
INIT
chmod +x "$MNTDIR/init"

umount "$MNTDIR"
rmdir "$MNTDIR"
rm -rf "$TMPDIR"

echo "==> LFS image ready: $IMG ($(du -h "$IMG" | cut -f1))"
echo "    Boot with: ROOTIMG=target/boot/lfs.img scripts/test.sh"
