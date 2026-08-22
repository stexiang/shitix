#!/bin/bash
# LFS 12.2 - Build Core Toolchain
# Builds Linux headers, Glibc, Binutils, and GCC (with GMP/MPFR/MPC/ISL)
# Uses host compiler, installs to $LFS with DESTDIR=$LFS
set -e

if [ -z "$LFS" ]; then
    echo "ERROR: \$LFS is not set"
    exit 1
fi

SRC=/build/sources
NPROC=$(nproc)

# Ubuntu GCC 默认开 -fstack-protector-strong / -fstack-clash-protection，会让
# glibc 的 syslog always_inline 编译失败（inlining failed）。显式关掉。
export CFLAGS="-O2 -fno-stack-protector -fno-stack-clash-protection"
export CXXFLAGS="-O2 -fno-stack-protector -fno-stack-clash-protection"

echo "=== Building LFS Toolchain (${NPROC} cores) ==="

# Helper to extract and enter source directory
extract() {
    local tarball="$1"
    local dir="${2:-}"

    cd $SRC
    echo "--- Extracting $tarball ---"
    tar xf "$tarball"

    if [ -n "$dir" ]; then
        cd "$dir"
    else
        # Auto-detect directory name
        local extracted=$(tar tf "$tarball" | head -1 | cut -d/ -f1)
        cd "$extracted"
    fi
}

cleanup() {
    local dir="$1"
    cd $SRC
    rm -rf "$dir"
}

# ============================================================
# 1. Linux API Headers
# ============================================================
echo "=== [1/6] Linux API Headers ==="
extract linux-6.10.5.tar.xz
make mrproper
make headers
find usr/include -type f ! -name '*.h' -delete
cp -rv usr/include $LFS/usr/
cleanup linux-6.10.5

# ============================================================
# 2. Glibc
# ============================================================
echo "=== [2/6] Glibc 2.40 ==="
extract glibc-2.40.tar.xz

# Create required symlinks for LSB compliance
case $(uname -m) in
    x86_64) ln -sfv ../lib/ld-linux-x86-64.so.2 $LFS/lib64/ld-lsb-x86-64.so.3 2>/dev/null || true ;;
esac

# Glibc requires the kernel headers to be in place
mkdir -v build
cd build

echo "rootsbindir=/usr/sbin" > configparms

../configure \
    --prefix=/usr \
    --disable-werror \
    --enable-kernel=4.19 \
    --disable-nscd \
    --with-headers=$LFS/usr/include \
    libc_cv_slibdir=/usr/lib

make -j$NPROC
make DESTDIR=$LFS install

# Install locale data for basic functionality
make DESTDIR=$LFS localedata/install-locales 2>/dev/null || true

# Configure the dynamic linker
cat > $LFS/etc/ld.so.conf << "EOF"
/usr/lib
/usr/local/lib
EOF

cd $SRC
cleanup glibc-2.40

# ============================================================
# 3. Zlib (needed by binutils and GCC)
# ============================================================
echo "=== [3/6] Zlib 1.3.1 ==="
extract zlib-1.3.1.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
# Remove static library
rm -fv $LFS/usr/lib/libz.a
cleanup zlib-1.3.1

# ============================================================
# 4. Binutils
# ============================================================
echo "=== [4/6] Binutils 2.43.1 ==="
extract binutils-2.43.1.tar.xz
mkdir -v build
cd build

../configure \
    --prefix=/usr \
    --sysconfdir=/etc \
    --enable-gold \
    --enable-ld=default \
    --enable-plugins \
    --enable-shared \
    --disable-werror \
    --enable-64-bit-bfd \
    --enable-new-dtags \
    --with-system-zlib \
    --enable-default-hash-style=gnu

make -j$NPROC tooldir=/usr
make DESTDIR=$LFS tooldir=/usr install
# Remove useless static libraries
rm -fv $LFS/usr/lib/lib{bfd,ctf,ctf-nobfd,gprofng,opcodes,sframe}.a

cd $SRC
cleanup binutils-2.43.1

# ============================================================
# 5. GCC Prerequisites (GMP, MPFR, MPC, ISL)
# ============================================================
echo "=== [5/6] GCC Prerequisites ==="

# GMP
echo "--- GMP 6.3.0 ---"
extract gmp-6.3.0.tar.xz
./configure \
    --prefix=/usr \
    --enable-cxx \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
cleanup gmp-6.3.0

# MPFR
echo "--- MPFR 4.2.1 ---"
extract mpfr-4.2.1.tar.xz
./configure \
    --prefix=/usr \
    --disable-static \
    --enable-thread-safe
make -j$NPROC
make DESTDIR=$LFS install
cleanup mpfr-4.2.1

# MPC
echo "--- MPC 1.3.1 ---"
extract mpc-1.3.1.tar.gz
./configure \
    --prefix=/usr \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
cleanup mpc-1.3.1

# ISL
echo "--- ISL 0.27 ---"
extract isl-0.27.tar.xz isl-0.27
./configure \
    --prefix=/usr \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
# Remove libtool archive
rm -fv $LFS/usr/lib/libisl.la
cleanup isl-0.27

# ============================================================
# 6. GCC
# ============================================================
echo "=== [6/6] GCC 14.2.0 ==="
extract gcc-14.2.0.tar.xz

# Fix an issue breaking libasan when building with glibc-2.40
sed -e '/static.*SANITIZER_HAS_STAT_H/s/444444/544544/' \
    -i libsanitizer/sanitizer_common/sanitizer_platform_limits_posix.cpp 2>/dev/null || true

mkdir -v build
cd build

# GCC needs to find the libraries we just built
export LIBRARY_PATH=$LFS/usr/lib:$LIBRARY_PATH
export C_INCLUDE_PATH=$LFS/usr/include
export CPLUS_INCLUDE_PATH=$LFS/usr/include

SED=sed \
../configure \
    --prefix=/usr \
    --enable-languages=c,c++ \
    --enable-default-pie \
    --enable-default-ssp \
    --enable-host-shared \
    --disable-multilib \
    --disable-bootstrap \
    --disable-fixincludes \
    --with-system-zlib \
    --with-gmp=$LFS/usr \
    --with-mpfr=$LFS/usr \
    --with-mpc=$LFS/usr \
    --with-isl=$LFS/usr

make -j$NPROC
make DESTDIR=$LFS install

# Create required symlinks
ln -svf gcc $LFS/usr/bin/cc
ln -svf gcc $LFS/usr/bin/x86_64-pc-linux-gnu-gcc

# Install LTO plugin for binutils
install -v -dm755 $LFS/usr/lib/bfd-plugins
ln -sfv ../../libexec/gcc/x86_64-pc-linux-gnu/14.2.0/liblto_plugin.so \
    $LFS/usr/lib/bfd-plugins/ 2>/dev/null || true

# Move misplaced file
mkdir -pv $LFS/usr/share/gdb/auto-load/usr/lib
mv -fv $LFS/usr/lib/*gdb.py $LFS/usr/share/gdb/auto-load/usr/lib/ 2>/dev/null || true

unset LIBRARY_PATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH

cd $SRC
cleanup gcc-14.2.0

echo ""
echo "=== Toolchain build complete ==="
echo "  Linux Headers: installed to $LFS/usr/include"
echo "  Glibc 2.40:    installed to $LFS/usr/lib"
echo "  Binutils 2.43: installed to $LFS/usr/bin"
echo "  GCC 14.2.0:    installed to $LFS/usr/bin"
