#!/bin/bash
# Create a disk image from the LFS rootfs tarball.
# Must be run as root (needs loop mount and mknod).
set -e
cd "$(dirname "$0")"

IMG_SIZE=1024  # MB
IMG_FILE="lfs-full.img"
ROOTFS="rootfs.tar.gz"

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: This script must be run as root (needs loop mount and mknod)."
    echo "Usage: sudo bash $0"
    exit 1
fi

if [ ! -f "$ROOTFS" ]; then
    echo "ERROR: $ROOTFS not found. Run build.sh first."
    exit 1
fi

echo "Creating ${IMG_SIZE}MB disk image..."
dd if=/dev/zero of=$IMG_FILE bs=1M count=$IMG_SIZE status=progress

echo "Formatting as ext4..."
mkfs.ext4 -F -O ^64bit,^huge_file,^metadata_csum -L lfs-root $IMG_FILE

echo "Mounting and extracting rootfs..."
MNT=$(mktemp -d)
mount -o loop $IMG_FILE $MNT

tar xzf $ROOTFS -C $MNT

# Create essential device nodes
echo "Creating device nodes..."
mknod -m 666 $MNT/dev/null    c 1 3
mknod -m 666 $MNT/dev/zero    c 1 5
mknod -m 666 $MNT/dev/full    c 1 7
mknod -m 666 $MNT/dev/random  c 1 8
mknod -m 666 $MNT/dev/urandom c 1 9
mknod -m 666 $MNT/dev/tty     c 5 0
mknod -m 622 $MNT/dev/console c 5 1
mknod -m 666 $MNT/dev/ptmx    c 5 2

# Create pts directory for pseudo-terminals
mkdir -pv $MNT/dev/pts
mkdir -pv $MNT/dev/shm

# Ensure proper permissions
chmod 1777 $MNT/tmp
chmod 1777 $MNT/var/tmp
chmod 0750 $MNT/root

sync
umount $MNT
rmdir $MNT

echo ""
echo "Done: $IMG_FILE ($(du -h $IMG_FILE | cut -f1))"
echo ""
echo "To test with QEMU:"
echo "  qemu-system-x86_64 -hda $IMG_FILE -m 512 -nographic -append 'console=ttyS0 root=/dev/hda1'"
