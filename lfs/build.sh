#!/bin/bash
# LFS Master Build Script
# Builds the complete LFS rootfs via Docker and extracts the tarball.
set -e
cd "$(dirname "$0")"

echo "============================================"
echo "  Shitix LFS Rootfs Builder"
echo "============================================"
echo ""
echo "Building LFS rootfs via Docker..."
echo "This will take a LONG time (1-3 hours depending on hardware)."
echo ""

# Build the Docker image
docker build -t shitix-lfs . --progress=plain 2>&1 | tee build.log
BUILD_EXIT=${PIPESTATUS[0]}

if [ $BUILD_EXIT -ne 0 ]; then
    echo ""
    echo "ERROR: Docker build failed! Check build.log for details."
    exit 1
fi

echo ""
echo "Extracting rootfs tarball..."

# Clean up any previous extraction container
docker rm -f lfs-extract 2>/dev/null || true

# Create a container and extract the tarball
docker create --name lfs-extract shitix-lfs true
docker cp lfs-extract:/rootfs.tar.gz ./rootfs.tar.gz
docker rm lfs-extract

echo ""
echo "============================================"
echo "  Build complete!"
echo "============================================"
echo ""
echo "  Rootfs tarball: $(pwd)/rootfs.tar.gz"
echo "  Size: $(du -h rootfs.tar.gz | cut -f1)"
echo ""
echo "  Next steps:"
echo "    sudo bash create-image.sh    # Create disk image"
echo ""
