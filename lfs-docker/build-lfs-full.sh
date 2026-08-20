#!/bin/bash
# Master LFS build script for shitix kernel - with multicore & direct image creation
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
IMG="${ROOT_DIR}/target/boot/lfs-full.img"
DOCKER_TAG="shitix-lfs-full"
IMG_SIZE_MB="${1:-512}"
JOBS=$(nproc)
REBUILD=false

# Parse args
while [[ $# -gt 0 ]]; do
    case "$1" in
        --jobs) JOBS="$2"; shift 2 ;;
        --rebuild) REBUILD=true; shift ;;
        --image-size) IMG_SIZE_MB="$2"; shift 2 ;;
        *) shift ;;
    esac
done
[[ "$JOBS" =~ ^[0-9]+$ ]] || JOBS=$(nproc)

cd "$SCRIPT_DIR"

echo "========================================="
echo "  shitix Full LFS Build"
echo "  Image size: ${IMG_SIZE_MB}MB (will be auto‑sized)"
echo "  Parallel jobs: $JOBS"
echo "========================================="

# Step 1: Build Docker image with multicore
echo ""
echo "=== Step 1: Building LFS Docker image (using $JOBS cores) ==="
export DOCKER_BUILDKIT=1
BUILD_ARGS=(--build-arg JOBS="$JOBS" --build-arg MAKEFLAGS="-j$JOBS")
if docker image inspect "$DOCKER_TAG" &>/dev/null && [[ "$REBUILD" != true ]]; then
    echo "Docker image exists, skipping build. Use --rebuild to force."
else
    docker build "${BUILD_ARGS[@]}" -f Dockerfile.full -t "$DOCKER_TAG" .
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

# ---- Prepare device nodes and init inside TMPDIR ----
echo "=== Preparing device nodes and /init ==="
mkdir -p "$TMPDIR/proc" "$TMPDIR/tmp" "$TMPDIR/run" "$TMPDIR/sys" "$TMPDIR/dev" 2>/dev/null || true
chmod 1777 "$TMPDIR/tmp"
mknod "$TMPDIR/dev/null" c 1 3 2>/dev/null || true
mknod "$TMPDIR/dev/zero" c 1 5 2>/dev/null || true
mknod "$TMPDIR/dev/tty" c 5 0 2>/dev/null || true
mknod "$TMPDIR/dev/console" c 5 1 2>/dev/null || true
chmod 666 "$TMPDIR/dev/null" "$TMPDIR/dev/zero" "$TMPDIR/dev/tty" 2>/dev/null || true

# Compile init and copy into TMPDIR
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

cp /tmp/init_lfs_full "$TMPDIR/init"
chmod +x "$TMPDIR/init"

# Step 3: Create ext2 image directly from TMPDIR (no mount!)
echo ""
echo "=== Step 3: Creating ext2 image (direct) ==="
mkdir -p "$(dirname "$IMG")"

# Compute needed image size: rootfs size + 20% overhead + 64MB safety
ROOTFS_BYTES=$(du -sb "$TMPDIR" | awk '{print $1}')
IMG_SIZE_BYTES=$((ROOTFS_BYTES * 12 / 10 + 64 * 1024 * 1024))
IMG_SIZE_MB=$(( (IMG_SIZE_BYTES + 1048575) / 1048576 ))   # round up to MB
echo "Image size: ${IMG_SIZE_MB}MB"

# Create a sparse file of that size, then format with -d
truncate -s "${IMG_SIZE_MB}M" "$IMG"
mkfs.ext2 -F -b 4096 -d "$TMPDIR" "$IMG" 2>&1 | grep -v "discarding" || true

# Clean up
rm -rf "$TMPDIR"
rm -f /tmp/init_lfs_full /tmp/init_lfs_full.c

echo ""
echo "========================================="
echo "  LFS image built: $IMG"
echo "  Size: $(du -h "$IMG" | cut -f1)"
echo ""
echo "  Boot with:"
echo "    ROOTIMG=target/boot/lfs-full.img scripts/test.sh --features extra-drivers --release"
echo "========================================="
