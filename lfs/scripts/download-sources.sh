#!/bin/bash
set -e

SOURCES=/build/sources
mkdir -pv $SOURCES
cd $SOURCES

download() {
    local url="$1"
    local file="$(basename "$url")"

    if [ -f "$file" ]; then
        echo "Already downloaded: $file"
        return 0
    fi

    for attempt in 1 2 3; do
        echo "Downloading $file (attempt $attempt)..."
        if wget --no-check-certificate -q --show-progress -O "$file.tmp" "$url"; then
            mv "$file.tmp" "$file"
            echo "OK: $file"
            return 0
        fi
        echo "Attempt $attempt failed for $file"
        rm -f "$file.tmp"
        sleep 2
    done

    echo "FAILED to download: $url"
    return 1
}

echo "=== Downloading LFS 12.2 Source Packages ==="

# Linux kernel (headers only)
download https://www.kernel.org/pub/linux/kernel/v6.x/linux-6.10.5.tar.xz

# Glibc
download https://ftp.gnu.org/gnu/glibc/glibc-2.40.tar.xz

# Binutils
download https://ftp.gnu.org/gnu/binutils/binutils-2.43.1.tar.xz

# GCC and dependencies
download https://ftp.gnu.org/gnu/gcc/gcc-14.2.0/gcc-14.2.0.tar.xz
download https://ftp.gnu.org/gnu/gmp/gmp-6.3.0.tar.xz
download https://ftp.gnu.org/gnu/mpfr/mpfr-4.2.1.tar.xz
download https://ftp.gnu.org/gnu/mpc/mpc-1.3.1.tar.gz
download https://libisl.sourceforge.io/isl-0.27.tar.xz

# Man pages and standards
download https://www.kernel.org/pub/linux/docs/man-pages/man-pages-6.9.1.tar.xz
download https://github.com/Mic92/iana-etc/releases/download/20240806/iana-etc-20240806.tar.gz

# Compression libraries
download https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.xz
download https://www.sourceware.org/pub/bzip2/bzip2-1.0.8.tar.gz
download https://github.com/tukaani-project/xz/releases/download/v5.6.2/xz-5.6.2.tar.xz
download https://github.com/facebook/zstd/releases/download/v1.5.6/zstd-1.5.6.tar.gz
download https://github.com/lz4/lz4/releases/download/v1.10.0/lz4-1.10.0.tar.gz

# Core utilities and tools
download https://astron.com/pub/file/file-5.45.tar.gz
download https://ftp.gnu.org/gnu/readline/readline-8.2.13.tar.gz
download https://ftp.gnu.org/gnu/m4/m4-1.4.19.tar.xz
download https://github.com/gavinhoward/bc/releases/download/6.7.6/bc-6.7.6.tar.xz
download https://github.com/westes/flex/releases/download/v2.6.4/flex-2.6.4.tar.gz
download https://ftp.gnu.org/gnu/ncurses/ncurses-6.5.tar.gz

# Test infrastructure
download https://downloads.sourceforge.net/tcl/tcl8.6.14-src.tar.gz
download https://prdownloads.sourceforge.net/expect/expect5.45.4.tar.gz
download https://ftp.gnu.org/gnu/dejagnu/dejagnu-1.6.3.tar.gz
download https://github.com/libcheck/check/releases/download/0.15.2/check-0.15.2.tar.gz

# Package config
download https://distfiles.ariadne.space/pkgconf/pkgconf-2.3.0.tar.xz

# Permissions and security
download https://download.savannah.gnu.org/releases/acl/acl-2.3.2.tar.xz
download https://download.savannah.gnu.org/releases/attr/attr-2.5.2.tar.xz
download https://www.kernel.org/pub/linux/libs/security/linux-privs/libcap2/libcap-2.70.tar.xz
download https://github.com/shadow-maint/shadow/releases/download/4.16.0/shadow-4.16.0.tar.xz
download https://github.com/besser82/libxcrypt/releases/download/v4.4.36/libxcrypt-4.4.36.tar.xz

# Text processing
download https://ftp.gnu.org/gnu/sed/sed-4.9.tar.xz
download https://ftp.gnu.org/gnu/grep/grep-3.11.tar.xz
download https://ftp.gnu.org/gnu/gawk/gawk-5.3.0.tar.xz
download https://ftp.gnu.org/gnu/bison/bison-3.8.2.tar.xz
download https://ftp.gnu.org/gnu/gettext/gettext-0.22.5.tar.xz
download https://ftp.gnu.org/gnu/diffutils/diffutils-3.10.tar.xz

# Shell
download https://ftp.gnu.org/gnu/bash/bash-5.2.32.tar.gz

# Build tools
download https://ftp.gnu.org/gnu/libtool/libtool-2.4.7.tar.xz
download https://ftp.gnu.org/gnu/gdbm/gdbm-1.24.tar.gz
download https://ftp.gnu.org/gnu/gperf/gperf-3.1.tar.gz
download https://github.com/libexpat/libexpat/releases/download/R_2_6_2/expat-2.6.2.tar.xz
download https://ftp.gnu.org/gnu/autoconf/autoconf-2.72.tar.xz
download https://ftp.gnu.org/gnu/automake/automake-1.17.tar.xz
download https://ftp.gnu.org/gnu/make/make-4.4.1.tar.gz
download https://ftp.gnu.org/gnu/patch/patch-2.7.6.tar.xz
download https://ftp.gnu.org/gnu/texinfo/texinfo-7.1.tar.xz

# Networking
download https://ftp.gnu.org/gnu/inetutils/inetutils-2.5.tar.xz

# Pager
download https://www.greenwoodsoftware.com/less/less-661.tar.gz

# Languages
download https://www.cpan.org/src/5.0/perl-5.40.0.tar.xz
download https://www.python.org/ftp/python/3.12.5/Python-3.12.5.tar.xz

# Crypto
download https://github.com/openssl/openssl/releases/download/openssl-3.3.1/openssl-3.3.1.tar.gz
download https://github.com/libffi/libffi/releases/download/v3.4.6/libffi-3.4.6.tar.gz

# System utilities
download https://www.kernel.org/pub/linux/utils/kernel/kmod/kmod-33.tar.xz
download https://ftp.gnu.org/gnu/coreutils/coreutils-9.5.tar.xz
download https://ftp.gnu.org/gnu/findutils/findutils-4.10.0.tar.xz
download https://sourceforge.net/projects/psmisc/files/psmisc/psmisc-23.7.tar.xz
download https://sourceforge.net/projects/procps-ng/files/Production/procps-ng-4.0.4.tar.xz
download https://www.kernel.org/pub/linux/utils/util-linux/v2.40/util-linux-2.40.2.tar.xz

# Archiving
download https://ftp.gnu.org/gnu/tar/tar-1.35.tar.xz
download https://ftp.gnu.org/gnu/gzip/gzip-1.13.tar.xz

# Filesystem
download https://www.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v1.47.1/e2fsprogs-1.47.1.tar.gz
download https://download.savannah.gnu.org/releases/libpipeline/libpipeline-1.5.7.tar.gz

# Keyboard
download https://www.kernel.org/pub/linux/utils/kbd/kbd-2.6.4.tar.xz

# Documentation
download https://ftp.gnu.org/gnu/groff/groff-1.23.0.tar.gz
download https://download.savannah.gnu.org/releases/man-db/man-db-2.12.1.tar.xz

# Editor
download https://github.com/vim/vim/archive/v9.1.0660/vim-9.1.0660.tar.gz

# Perl XML module (needed by intltool)
download https://cpan.metacpan.org/authors/id/T/TO/TODDR/XML-Parser-2.47.tar.gz

# Intltool
download https://launchpad.net/intltool/trunk/0.51.0/+download/intltool-0.51.0.tar.gz

# IPRoute2
download https://www.kernel.org/pub/linux/utils/net/iproute2/iproute2-6.10.0.tar.xz

echo ""
echo "=== All downloads complete ==="
ls -la $SOURCES/ | head -5
echo "... ($(ls $SOURCES/ | wc -l) files total)"
