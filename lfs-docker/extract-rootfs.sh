#!/bin/bash
set -e

# Extract rootfs from Docker image and create ext2 disk image

DOCKER_IMAGE="${1:-shitix-lfs}"
OUTPUT_IMG="${2:-target/boot/lfs-rootfs.img}"

echo "Extracting rootfs from Docker image: $DOCKER_IMAGE"

# Create a temporary container to extract the rootfs
CONTAINER_ID=$(docker create "$DOCKER_IMAGE")
echo "Created temporary container: $CONTAINER_ID"

# Extract rootfs directory from the container
mkdir -p target/boot
docker cp "$CONTAINER_ID:/lfs/rootfs" target/boot/rootfs-staging

# Clean up the container
docker rm "$CONTAINER_ID"

# Calculate required size (rootfs size + 20% overhead)
ROOTFS_SIZE=$(du -sb target/boot/rootfs-staging | awk '{print $1}')
IMG_SIZE=$(( ROOTFS_SIZE * 12 / 10 ))  # 120% of actual size
IMG_SIZE_MB=$(( IMG_SIZE / 1024 / 1024 + 1 ))

echo "Rootfs size: $((ROOTFS_SIZE / 1024 / 1024)) MB, creating ${IMG_SIZE_MB}MB image"

# Create ext2 image
dd if=/dev/zero of="$OUTPUT_IMG" bs=1M count="$IMG_SIZE_MB"
mkfs.ext2 -F "$OUTPUT_IMG"

# Mount and copy files
MOUNT_POINT=$(mktemp -d)
mount -o loop "$OUTPUT_IMG" "$MOUNT_POINT"

echo "Copying rootfs to image..."
cp -a target/boot/rootfs-staging/* "$MOUNT_POINT/"

# Unmount
umount "$MOUNT_POINT"
rmdir "$MOUNT_POINT"

# Clean up staging
rm -rf target/boot/rootfs-staging

echo "LFS rootfs image created: $OUTPUT_IMG"
ls -lh "$OUTPUT_IMG"
