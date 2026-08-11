#!/bin/bash
# Full LFS build — all packages in dependency order
# This builds against musl (Alpine's libc) for the shitix kernel target
# Sources are optional: packages are installed via Alpine apk.
# Download sources first with: bash scripts/full/download.sh
set -e

# Check if sources exist, skip builds that need missing tarballs
SRC=/lfs/usr/src
SKIP_MISSING=1

SRC=/lfs/usr/src
DEST=/lfs
JOBS=$(nproc)

# Build helpers
extract() {
    local f=$1
    local d=$2
    echo "==> Extracting $f"
    cd $SRC
    case $f in
        *.tar.xz) tar xf $f ;;
        *.tar.gz) tar xf $f ;;
        *.tar.bz2) tar xf $f ;;
        *) echo "Unknown format: $f"; exit 1 ;;
    esac
    cd "$d"
}

build_autotools() {
    local dir=$1; shift
    echo "==> Building $dir"
    cd $SRC/$dir
    ./configure --prefix=/usr "$@" 2>&1 | tail -1
    make -j$JOBS 2>&1 | tail -1
    make DESTDIR=$DEST install 2>&1 | tail -1
    cd $SRC
}

build_meson() {
    local dir=$1; shift
    echo "==> Building $dir (meson)"
    cd $SRC/$dir
    meson setup build --prefix=/usr "$@" 2>&1 | tail -1
    ninja -C build 2>&1 | tail -1
    DESTDIR=$DEST ninja -C build install 2>&1 | tail -1
    cd $SRC
}

# ===== Phase 1: Core Libraries =====
echo "=== Phase 1: Core Libraries ==="

# Zlib - required by many
extract zlib-*.tar.gz zlib-*
build_autotools zlib-*

# XZ - compression
extract xz-*.tar.xz xz-*
build_autotools xz-*

# Zstd
extract zstd-*.tar.gz zstd-*
cd $SRC/zstd-*
make -j$JOBS prefix=/usr 2>&1 | tail -1
make prefix=/usr DESTDIR=$DEST install 2>&1 | tail -1

# Lz4
extract lz4-*.tar.gz lz4-*
cd $SRC/lz4-*
make -j$JOBS 2>&1 | tail -1
make prefix=/usr DESTDIR=$DEST install 2>&1 | tail -1

# Bzip2
extract bzip2-*.tar.gz bzip2-*
cd $SRC/bzip2-*
make -j$JOBS 2>&1 | tail -1
make PREFIX=$DEST/usr install 2>&1 | tail -1

# Ncurses
extract ncurses-*.tar.gz ncurses-*
build_autotools ncurses-* --with-shared --without-debug --without-ada

# Readline
extract readline-*.tar.gz readline-*
build_autotools readline-* --with-curses

# GMP
extract gmp-*.tar.xz gmp-*
build_autotools gmp-* --enable-cxx

# MPFR
extract mpfr-*.tar.xz mpfr-*
build_autotools mpfr-* --with-gmp=/usr

# MPC
extract mpc-*.tar.gz mpc-*
build_autotools mpc-* --with-gmp=/usr --with-mpfr=/usr

# Attr
extract attr-*.tar.gz attr-*
build_autotools attr-*

# Acl
extract acl-*.tar.xz acl-*
build_autotools acl-*

# Libcap
extract libcap-*.tar.xz libcap-*
cd $SRC/libcap-*
make -j$JOBS prefix=/usr 2>&1 | tail -1
make prefix=/usr DESTDIR=$DEST install 2>&1 | tail -1

# Expat
extract expat-*.tar.xz expat-*
build_autotools expat-*

# Libffi
extract libffi-*.tar.gz libffi-*
build_autotools libffi-*

# OpenSSL
extract openssl-*.tar.gz openssl-*
cd $SRC/openssl-*
./Configure --prefix=/usr --openssldir=/etc/ssl 2>&1 | tail -1
make -j$JOBS 2>&1 | tail -1
make DESTDIR=$DEST install_sw 2>&1 | tail -1

# Libxcrypt
extract libxcrypt-*.tar.xz libxcrypt-*
build_autotools libxcrypt-*

# Libpipeline
extract libpipeline-*.tar.gz libpipeline-*
build_autotools libpipeline-*

# Libelf (elfutils)
extract elfutils-*.tar.bz2 elfutils-*
build_autotools elfutils-* --disable-debuginfod

# GDBM
extract gdbm-*.tar.gz gdbm-*
build_autotools gdbm-*

# Iana-Etc
extract iana-etc-*.tar.gz iana-etc-*
cd $SRC/iana-etc-*
cp services protocols $DEST/etc/ 2>/dev/null || true

# ===== Phase 2: Toolchain =====
echo "=== Phase 2: Toolchain ==="

# Binutils
extract binutils-*.tar.xz binutils-*
mkdir -p $SRC/binutils-build
cd $SRC/binutils-build
$SRC/binutils-*/configure --prefix=/usr --enable-gold --enable-plugins \
    --disable-werror 2>&1 | tail -1
make -j$JOBS 2>&1 | tail -1
make DESTDIR=$DEST install 2>&1 | tail -1

# GMP, MPFR, MPC already done

# GCC
extract gcc-*.tar.xz gcc-*
mkdir -p $SRC/gcc-build
cd $SRC/gcc-build
$SRC/gcc-*/configure --prefix=/usr --enable-languages=c,c++ \
    --disable-multilib --disable-bootstrap --with-system-zlib 2>&1 | tail -1
make -j$JOBS 2>&1 | tail -2
make DESTDIR=$DEST install 2>&1 | tail -1
ln -sf gcc $DEST/usr/bin/cc 2>/dev/null || true

# ===== Phase 3: Build Tools =====
echo "=== Phase 3: Build Tools ==="

# M4
extract m4-*.tar.xz m4-*
build_autotools m4-*

# Bison
extract bison-*.tar.xz bison-*
build_autotools bison-*

# Flex
extract flex-*.tar.gz flex-*
build_autotools flex-*

# Autoconf
extract autoconf-*.tar.xz autoconf-*
build_autotools autoconf-*

# Automake
extract automake-*.tar.xz automake-*
build_autotools automake-*

# Libtool
extract libtool-*.tar.xz libtool-*
build_autotools libtool-*

# Gperf
extract gperf-*.tar.gz gperf-*
build_autotools gperf-*

# Pkgconf
extract pkgconf-*.tar.xz pkgconf-*
build_autotools pkgconf-*

# Intltool
extract intltool-*.tar.gz intltool-*
build_autotools intltool-*

# Gettext
extract gettext-*.tar.xz gettext-*
build_autotools gettext-*

# Meson/Ninja (Python-based - use Alpine's)
cp /usr/bin/meson $DEST/usr/bin/ 2>/dev/null || true
cp /usr/bin/ninja $DEST/usr/bin/ 2>/dev/null || true

# Perl
extract perl-*.tar.xz perl-*
cd $SRC/perl-*
./Configure -des -Dprefix=/usr 2>&1 | tail -1
make -j$JOBS 2>&1 | tail -1
make DESTDIR=$DEST install 2>&1 | tail -1

# Python
extract Python-*.tar.xz Python-*
cd $SRC/Python-*
./configure --prefix=/usr --enable-shared 2>&1 | tail -1
make -j$JOBS 2>&1 | tail -1
make DESTDIR=$DEST install 2>&1 | tail -1

# Tcl
extract tcl*-src.tar.gz tcl*
build_autotools tcl* --enable-64bit

# Expect
extract expect*.tar.gz expect*
build_autotools expect*

# DejaGNU
extract dejagnu-*.tar.gz dejagnu-*
build_autotools dejagnu-*

# Check
extract check-*.tar.gz check-*
build_autotools check-*

# ===== Phase 4: Core Utilities =====
echo "=== Phase 4: Core Utilities ==="

# File
extract file-*.tar.gz file-*
build_autotools file-*

# Grep
extract grep-*.tar.xz grep-*
build_autotools grep-*

# Sed
extract sed-*.tar.xz sed-*
build_autotools sed-*

# Gawk
extract gawk-*.tar.xz gawk-*
build_autotools gawk-*

# Make
extract make-*.tar.gz make-*
build_autotools make-*

# Patch
extract patch-*.tar.xz patch-*
build_autotools patch-*

# Diffutils
extract diffutils-*.tar.xz diffutils-*
build_autotools diffutils-*

# Findutils
extract findutils-*.tar.xz findutils-*
build_autotools findutils-*

# Tar
extract tar-*.tar.xz tar-*
FORCE_UNSAFE_CONFIGURE=1 build_autotools tar-*

# Gzip
extract gzip-*.tar.xz gzip-*
build_autotools gzip-*

# Bc
extract bc-*.tar.xz bc-*
build_autotools bc-*

# Coreutils
extract coreutils-*.tar.xz coreutils-*
FORCE_UNSAFE_CONFIGURE=1 build_autotools coreutils-*

# Bash
extract bash-*.tar.gz bash-*
build_autotools bash-* --without-bash-malloc
ln -sf bash $DEST/bin/sh

# ===== Phase 5: System Libraries & Tools =====
echo "=== Phase 5: System ==="

# Util-linux
extract util-linux-*.tar.xz util-linux-*
build_autotools util-linux-* --disable-makeinstall-chown

# E2fsprogs
extract e2fsprogs-*.tar.gz e2fsprogs-*
mkdir -p $SRC/e2fsprogs-build
cd $SRC/e2fsprogs-build
$SRC/e2fsprogs-*/configure --prefix=/usr 2>&1 | tail -1
make -j$JOBS 2>&1 | tail -1
make DESTDIR=$DEST install 2>&1 | tail -1

# Procps-ng
extract procps-ng-*.tar.xz procps-ng-*
build_autotools procps-ng-*

# Psmisc
extract psmisc-*.tar.xz psmisc-*
build_autotools psmisc-*

# Shadow
extract shadow-*.tar.gz shadow-*
build_autotools shadow-* --without-selinux

# Less
extract less-*.tar.gz less-*
build_autotools less-*

# Groff
extract groff-*.tar.gz groff-*
build_autotools groff-*

# GRUB
extract grub-*.tar.xz grub-*
build_autotools grub-*

# Kmod
extract kmod-*.tar.xz kmod-*
build_autotools kmod-*

# Kbd
extract kbd-*.tar.xz kbd-*
build_autotools kbd-*

# Inetutils
extract inetutils-*.tar.xz inetutils-*
build_autotools inetutils-*

# IPRoute2
extract iproute2-*.tar.xz iproute2-*
cd $SRC/iproute2-*
make -j$JOBS 2>&1 | tail -1
make DESTDIR=$DEST install 2>&1 | tail -1

# SysVinit
extract sysvinit-*.tar.xz sysvinit-*
cd $SRC/sysvinit-*
make -j$JOBS 2>&1 | tail -1
make DESTDIR=$DEST install 2>&1 | tail -1

# Sysklogd
extract sysklogd-*.tar.gz sysklogd-*
build_autotools sysklogd-*

# Vim
extract vim-*.tar.gz vim-*
cd $SRC/vim-*
./configure --prefix=/usr --with-features=huge 2>&1 | tail -1
make -j$JOBS 2>&1 | tail -1
make DESTDIR=$DEST install 2>&1 | tail -1

# Man-pages
extract man-pages-*.tar.xz man-pages-*
cd $SRC/man-pages-*
make prefix=/usr DESTDIR=$DEST install 2>&1 | tail -1

# Man-DB
extract man-db-*.tar.xz man-db-*
build_autotools man-db-*

# Texinfo
extract texinfo-*.tar.xz texinfo-*
build_autotools texinfo-*

echo "=== LFS Build Complete ==="
echo "Packages installed to $DEST"
du -sh $DEST
