# LFS 12.2 Rootfs Builder
# Builds a complete Linux From Scratch system inside Docker,
# producing a rootfs tarball that can be loaded onto a disk image.
#
# Usage:
#   docker build -t shitix-lfs . --progress=plain
#   docker create --name lfs-extract shitix-lfs true
#   docker cp lfs-extract:/rootfs.tar.gz ./rootfs.tar.gz
#   docker rm lfs-extract

FROM ubuntu:24.04 AS builder

ENV LFS=/lfs
ENV LC_ALL=POSIX
ENV PATH=/usr/bin:/usr/sbin:/bin:/sbin
ENV MAKEFLAGS="-j$(nproc)"

# Install build dependencies
RUN apt-get update && apt-get install -y \
    build-essential bison flex gawk texinfo \
    python3 wget file m4 bc libelf-dev \
    automake autoconf libtool pkg-config \
    gettext zip unzip xz-utils gzip bzip2 \
    zlib1g-dev libncurses-dev patch diffutils \
    perl libperl-dev python3-dev \
    ca-certificates curl rsync cpio \
    && rm -rf /var/lib/apt/lists/*

# Create LFS directory structure with merged /usr layout
# 注意：不用 {a,b,c} 花括号展开——Ubuntu 的 /bin/sh 是 dash，不支持，
# 会把字面量目录名建出来。全部写成显式列表。
RUN mkdir -pv $LFS/usr/bin $LFS/usr/include $LFS/usr/lib $LFS/usr/libexec \
        $LFS/usr/sbin $LFS/usr/share $LFS/usr/src \
    && mkdir -pv $LFS/usr/share/doc $LFS/usr/share/info $LFS/usr/share/locale \
        $LFS/usr/share/man $LFS/usr/share/misc $LFS/usr/share/terminfo \
        $LFS/usr/share/zoneinfo \
    && mkdir -pv $LFS/usr/share/man/man1 $LFS/usr/share/man/man2 \
        $LFS/usr/share/man/man3 $LFS/usr/share/man/man4 \
        $LFS/usr/share/man/man5 $LFS/usr/share/man/man6 \
        $LFS/usr/share/man/man7 $LFS/usr/share/man/man8 \
    && ln -sv usr/bin  $LFS/bin \
    && ln -sv usr/lib  $LFS/lib \
    && ln -sv usr/lib  $LFS/lib64 \
    && ln -sv usr/sbin $LFS/sbin \
    && mkdir -pv $LFS/boot $LFS/dev $LFS/etc $LFS/home $LFS/media $LFS/mnt \
        $LFS/opt $LFS/proc $LFS/root $LFS/run $LFS/srv $LFS/sys \
        $LFS/tmp $LFS/var \
    && mkdir -pv $LFS/var/cache $LFS/var/lib $LFS/var/local $LFS/var/log \
        $LFS/var/mail $LFS/var/opt $LFS/var/run $LFS/var/spool $LFS/var/tmp \
    && mkdir -pv $LFS/etc/sysconfig $LFS/etc/default \
    && chmod -v 1777 $LFS/tmp $LFS/var/tmp \
    && ln -sv /run $LFS/var/run \
    && ln -sv /run/lock $LFS/var/lock

# Copy build scripts, config, and patches
COPY scripts/ /build/scripts/
COPY config/ /build/config/
COPY patches/ /build/patches/

# 预下载的源码包（宿主机从国内镜像下载，容器内网络不可达上游源）。
# download-sources.sh 会跳过已存在的文件。
COPY sources/ /build/sources/

# Make all scripts executable
RUN chmod +x /build/scripts/*.sh

# Download all source packages
RUN bash /build/scripts/download-sources.sh

# Build the core toolchain (Linux headers, glibc, binutils, GCC)
RUN bash /build/scripts/build-toolchain.sh

# Build all remaining system packages
RUN bash /build/scripts/build-system.sh

# Configure the system (etc files, init, symlinks)
RUN bash /build/scripts/configure-system.sh

# Create the rootfs tarball
RUN cd $LFS && tar czf /rootfs.tar.gz .

# Final stage: just the tarball
FROM scratch
COPY --from=builder /rootfs.tar.gz /rootfs.tar.gz
