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
RUN mkdir -pv $LFS/usr/{bin,include,lib,libexec,sbin,share,src} \
    && mkdir -pv $LFS/usr/share/{doc,info,locale,man,misc,terminfo,zoneinfo} \
    && mkdir -pv $LFS/usr/share/man/man{1..8} \
    && ln -sv usr/bin  $LFS/bin \
    && ln -sv usr/lib  $LFS/lib \
    && ln -sv usr/lib  $LFS/lib64 \
    && ln -sv usr/sbin $LFS/sbin \
    && mkdir -pv $LFS/{boot,dev,etc,home,media,mnt,opt,proc,root,run,srv,sys,tmp,var} \
    && mkdir -pv $LFS/var/{cache,lib,local,log,mail,opt,run,spool,tmp} \
    && mkdir -pv $LFS/etc/{sysconfig,default} \
    && chmod -v 1777 $LFS/tmp $LFS/var/tmp \
    && ln -sv /run $LFS/var/run \
    && ln -sv /run/lock $LFS/var/lock

# Copy build scripts, config, and patches
COPY scripts/ /build/scripts/
COPY config/ /build/config/
COPY patches/ /build/patches/

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
