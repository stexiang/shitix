#!/bin/bash
# LFS Mirror Configuration — source this file to set download mirrors
# Reference: LFS Book §3.1 (Introduction) — https://www.linuxfromscratch.org/lfs/view/stable/chapter03/introduction.html
#
# Usage: source mirrors.sh [region]
#   region: cn | us | eu | default

set_mirrors() {
    local region="${1:-default}"

    case "$region" in
        cn)
            # China mirrors
            export LFS_MIRROR="https://mirrors.tuna.tsinghua.edu.cn/lfs/lfs-packages/12.2"
            export GNU_MIRROR="https://mirrors.tuna.tsinghua.edu.cn/gnu"
            export KERNEL_MIRROR="https://mirrors.tuna.tsinghua.edu.cn/kernel"
            # Alpine APK mirrors
            ALPINE_MAIN="https://mirrors.aliyun.com/alpine/v3.19/main"
            ALPINE_COMMUNITY="https://mirrors.aliyun.com/alpine/v3.19/community"
            ;;
        us)
            # US mirrors
            export LFS_MIRROR="https://ftp.osuosl.org/pub/lfs/lfs-packages/12.2"
            export GNU_MIRROR="https://ftp.gnu.org/gnu"
            export KERNEL_MIRROR="https://www.kernel.org/pub/linux"
            ALPINE_MAIN="https://dl-cdn.alpinelinux.org/alpine/v3.19/main"
            ALPINE_COMMUNITY="https://dl-cdn.alpinelinux.org/alpine/v3.19/community"
            ;;
        eu)
            # European mirrors
            export LFS_MIRROR="https://lfs.mirrors.hoobly.com/lfs-packages/12.2"
            export GNU_MIRROR="https://ftp.gnu.org/gnu"
            export KERNEL_MIRROR="https://www.kernel.org/pub/linux"
            ALPINE_MAIN="https://ftp.halifax.rwth-aachen.de/alpine/v3.19/main"
            ALPINE_COMMUNITY="https://ftp.halifax.rwth-aachen.de/alpine/v3.19/community"
            ;;
        *)
            # Default: Chinese mirrors (Tsinghua + Aliyun)
            export LFS_MIRROR="https://mirrors.tuna.tsinghua.edu.cn/lfs/lfs-packages/12.2"
            export GNU_MIRROR="https://mirrors.tuna.tsinghua.edu.cn/gnu"
            export KERNEL_MIRROR="https://mirrors.tuna.tsinghua.edu.cn/kernel"
            ALPINE_MAIN="https://mirrors.aliyun.com/alpine/v3.19/main"
            ALPINE_COMMUNITY="https://mirrors.aliyun.com/alpine/v3.19/community"
            ;;
    esac

    export ALPINE_MAIN ALPINE_COMMUNITY

    echo "=== LFS Mirrors ($region) ==="
    echo "  LFS:    $LFS_MIRROR"
    echo "  GNU:    $GNU_MIRROR"
    echo "  Kernel: $KERNEL_MIRROR"
    echo "  Alpine: $ALPINE_MAIN"
}

# Run with first argument or default
set_mirrors "${1:-default}"

# Extra: allow overriding specific mirrors
: "${LFS_MIRROR:=$LFS_MIRROR}"
: "${GNU_MIRROR:=$GNU_MIRROR}"
: "${KERNEL_MIRROR:=$KERNEL_MIRROR}"
