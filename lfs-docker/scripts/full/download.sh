#!/bin/bash
# Download all LFS 12.2 package sources with multi-mirror fallback
# LFS Book Reference: Chapter 3 (Packages and Patches)
#
# Tries up to 5 mirrors before giving up. Non-fatal by default
# (packages are installed via Alpine apk; sources are optional).
# Set STRICT=1 to fail on download errors.

SRC=/lfs/usr/src
mkdir -p "$SRC"
cd "$SRC"

# ---- Mirror Configuration (LFS §3.1) ----
[ -f /tmp/mirrors.sh ] && source /tmp/mirrors.sh 2>/dev/null || true
: "${LFS_MIRROR:=https://mirrors.tuna.tsinghua.edu.cn/lfs/lfs-packages/12.2}"
: "${GNU_MIRROR:=https://mirrors.tuna.tsinghua.edu.cn/gnu}"
: "${KERNEL_MIRROR:=https://mirrors.tuna.tsinghua.edu.cn/kernel}"
: "${GITHUB_MIRROR:=https://github.com}"
: "${STRICT:=0}"

# All mirrors to try (in order)
MIRRORS=(
    "$GNU_MIRROR"
    "https://ftp.gnu.org/gnu"
    "https://mirrors.ustc.edu.cn/gnu"
    "$LFS_MIRROR"
    "https://www.linuxfromscratch.org/lfs/downloads/stable"
)
KERNEL_MIRRORS=("$KERNEL_MIRROR" "https://www.kernel.org/pub/linux")
FAILED=0

# Try downloading from multiple mirrors
download() {
    local name="$1"
    local url="$2"
    local fname base_path
    fname=$(basename "$url")
    # Extract path after the domain for mirror reconstruction
    base_path=$(echo "$url" | sed 's|^https://[^/]*/||')

    if [ -f "$fname" ]; then
        echo "  [OK] $fname (cached)"
        return 0
    fi

    echo "  $name: $fname"

    # Try mirrors
    local mirrors=("${MIRRORS[@]}")
    if [[ "$url" == *kernel.org* ]]; then
        mirrors=("${KERNEL_MIRRORS[@]}")
    fi

    for mirror in "${mirrors[@]}"; do
        local try_url="$mirror/$base_path"
        if wget -q --timeout=15 --tries=2 "$try_url" 2>/dev/null; then
            echo "    -> $try_url"
            return 0
        fi
    done

    # Try original URL as last resort
    if wget -q --timeout=15 --tries=2 "$url" 2>/dev/null; then
        echo "    -> $url (direct)"
        return 0
    fi

    echo "  [WARN] $fname unavailable (non-fatal)"
    FAILED=$((FAILED + 1))
    return 1
}

echo "=== Downloading LFS 12.2 Package Sources ==="
echo "Mirrors: LFS=$LFS_MIRROR  GNU=$GNU_MIRROR  KERNEL=$KERNEL_MIRROR"
echo ""

# ---- Core toolchain (LFS Chapter 5-6) ----
echo "--- Toolchain ---"
download "binutils"  "$GNU_MIRROR/binutils/binutils-2.43.1.tar.xz"
download "gcc"       "$GNU_MIRROR/gcc/gcc-14.2.0/gcc-14.2.0.tar.xz"
# musl libc (Alpine already provides; download for reference)
# download "musl"      "https://musl.libc.org/releases/musl-1.2.5.tar.gz"
download "linux"     "$KERNEL_MIRROR/kernel/v6.x/linux-6.10.5.tar.xz"
download "m4"        "$GNU_MIRROR/m4/m4-1.4.19.tar.xz"
download "ncurses"   "$GNU_MIRROR/ncurses/ncurses-6.5.tar.gz"
download "bash"      "$GNU_MIRROR/bash/bash-5.2.32.tar.gz"
download "coreutils" "$GNU_MIRROR/coreutils/coreutils-9.5.tar.xz"
download "diffutils" "$GNU_MIRROR/diffutils/diffutils-3.10.tar.xz"
download "file"      "https://astron.com/pub/file/file-5.45.tar.gz"
download "findutils" "$GNU_MIRROR/findutils/findutils-4.10.0.tar.xz"
download "gawk"      "$GNU_MIRROR/gawk/gawk-5.3.0.tar.xz"
download "grep"      "$GNU_MIRROR/grep/grep-3.11.tar.xz"
download "gzip"      "$GNU_MIRROR/gzip/gzip-1.13.tar.xz"
download "make"      "$GNU_MIRROR/make/make-4.4.1.tar.gz"
download "patch"     "$GNU_MIRROR/patch/patch-2.7.6.tar.xz"
download "sed"       "$GNU_MIRROR/sed/sed-4.9.tar.xz"
download "tar"       "$GNU_MIRROR/tar/tar-1.35.tar.xz"
download "xz"        "$GITHUB_MIRROR/tukaani-project/xz/releases/download/v5.6.2/xz-5.6.2.tar.xz"

# ---- Libraries (LFS Chapter 8) ----
echo "--- Libraries ---"
download "gmp"       "$GNU_MIRROR/gmp/gmp-6.3.0.tar.xz"
download "mpfr"      "$GNU_MIRROR/mpfr/mpfr-4.2.1.tar.xz"
download "mpc"       "$GNU_MIRROR/mpc/mpc-1.3.1.tar.gz"
download "isl"       "https://libisl.sourceforge.io/isl-0.26.tar.xz"
download "zlib"      "https://zlib.net/fossils/zlib-1.3.1.tar.gz"
download "zstd"      "$GITHUB_MIRROR/facebook/zstd/releases/download/v1.5.6/zstd-1.5.6.tar.gz"
download "bzip2"     "https://sourceware.org/pub/bzip2/bzip2-1.0.8.tar.gz"
download "expat"     "$GITHUB_MIRROR/libexpat/libexpat/releases/download/R_2_6_2/expat-2.6.2.tar.xz"
download "libffi"    "$GITHUB_MIRROR/libffi/libffi/releases/download/v3.4.6/libffi-3.4.6.tar.gz"
download "libpipeline" "https://download.savannah.gnu.org/releases/libpipeline/libpipeline-1.5.7.tar.gz"
download "libtool"   "$GNU_MIRROR/libtool/libtool-2.4.7.tar.xz"
download "libxcrypt" "$GITHUB_MIRROR/besser82/libxcrypt/releases/download/v4.4.36/libxcrypt-4.4.36.tar.xz"
download "readline"  "$GNU_MIRROR/readline/readline-8.2.13.tar.gz"
download "gdbm"      "$GNU_MIRROR/gdbm/gdbm-1.24.tar.gz"
download "openssl"   "https://www.openssl.org/source/openssl-3.3.1.tar.gz"
download "lz4"       "$GITHUB_MIRROR/lz4/lz4/releases/download/v1.9.4/lz4-1.9.4.tar.gz"
download "acl"       "https://download.savannah.gnu.org/releases/acl/acl-2.3.2.tar.xz"
download "attr"      "https://download.savannah.gnu.org/releases/attr/attr-2.5.2.tar.gz"
download "libcap"    "https://www.kernel.org/pub/linux/libs/security/linux-privs/libcap2/libcap-2.70.tar.xz"
download "libelf (elfutils)" "https://sourceware.org/ftp/elfutils/0.191/elfutils-0.191.tar.bz2"

# ---- System utilities ----
echo "--- System ---"
download "util-linux" "$KERNEL_MIRROR/utils/util-linux/v2.40/util-linux-2.40.2.tar.xz"
download "e2fsprogs" "https://downloads.sourceforge.net/project/e2fsprogs/e2fsprogs/v1.47.1/e2fsprogs-1.47.1.tar.gz"
download "procps-ng"  "https://sourceforge.net/projects/procps-ng/files/Production/procps-ng-4.0.4.tar.xz"
download "psmisc"     "https://sourceforge.net/projects/psmisc/files/psmisc/psmisc-23.7.tar.xz"
download "shadow"     "$GITHUB_MIRROR/shadow-maint/shadow/releases/download/4.16.0/shadow-4.16.0.tar.gz"
download "sysklogd"   "$GITHUB_MIRROR/troglobit/sysklogd/releases/download/v2.5.2/sysklogd-2.5.2.tar.gz"
download "sysvinit"   "$GITHUB_MIRROR/slicer69/sysvinit/releases/download/3.09/sysvinit-3.09.tar.xz"
download "kmod"       "$KERNEL_MIRROR/utils/kernel/kmod/kmod-32.tar.xz"
download "kbd"        "$KERNEL_MIRROR/utils/kbd/kbd-2.6.4.tar.xz"
download "inetutils"  "$GNU_MIRROR/inetutils/inetutils-2.5.tar.xz"
download "iproute2"   "$KERNEL_MIRROR/utils/net/iproute2/iproute2-6.10.0.tar.xz"
download "less"       "https://www.greenwoodsoftware.com/less/less-661.tar.gz"
download "man-pages"  "$KERNEL_MIRROR/docs/man-pages/man-pages-6.06.tar.xz"
download "man-db"     "https://download.savannah.gnu.org/releases/man-db/man-db-2.12.1.tar.xz"
download "groff"      "$GNU_MIRROR/groff/groff-1.23.0.tar.gz"
download "texinfo"    "$GNU_MIRROR/texinfo/texinfo-7.1.tar.xz"
download "vim"        "$GITHUB_MIRROR/vim/vim/archive/v9.1.0678/vim-9.1.0678.tar.gz"
download "bc"         "$GITHUB_MIRROR/gavinhoward/bc/releases/download/6.7.6/bc-6.7.6.tar.xz"

# ---- Languages ----
echo "--- Languages ---"
download "python"     "https://www.python.org/ftp/python/3.12.5/Python-3.12.5.tar.xz"
download "perl"       "https://www.cpan.org/src/5.0/perl-5.40.0.tar.xz"
download "tcl"        "https://downloads.sourceforge.net/tcl/tcl8.6.14-src.tar.gz"
download "expect"     "https://downloads.sourceforge.net/tcl/expect5.45.4.tar.gz"

# ---- Build tools ----
echo "--- Build tools ---"
download "autoconf"  "$GNU_MIRROR/autoconf/autoconf-2.72.tar.xz"
download "automake"  "$GNU_MIRROR/automake/automake-1.17.tar.xz"
download "bison"     "$GNU_MIRROR/bison/bison-3.8.2.tar.xz"
download "flex"      "$GITHUB_MIRROR/westes/flex/releases/download/v2.6.4/flex-2.6.4.tar.gz"
download "gettext"   "$GNU_MIRROR/gettext/gettext-0.22.5.tar.xz"
download "gperf"     "$GNU_MIRROR/gperf/gperf-3.1.tar.gz"
download "intltool"  "https://launchpad.net/intltool/trunk/0.51.0/+download/intltool-0.51.0.tar.gz"
download "pkgconf"   "https://distfiles.ariadne.space/pkgconf/pkgconf-2.2.0.tar.xz"
download "meson"     "$GITHUB_MIRROR/mesonbuild/meson/releases/download/1.5.1/meson-1.5.1.tar.gz"
download "ninja"     "$GITHUB_MIRROR/ninja-build/ninja/archive/v1.12.1/ninja-1.12.1.tar.gz"
download "dejagnu"   "$GNU_MIRROR/dejagnu/dejagnu-1.6.3.tar.gz"
download "check"     "$GITHUB_MIRROR/libcheck/check/releases/download/0.15.2/check-0.15.2.tar.gz"

# ---- Misc ----
download "iana-etc"  "$GITHUB_MIRROR/Mic92/iana-etc/releases/download/20240814/iana-etc-20240814.tar.gz"
download "grub"      "$GNU_MIRROR/grub/grub-2.12.tar.xz"
download "perl-xml-parser" "https://cpan.metacpan.org/authors/id/T/TO/TODDR/XML-Parser-2.47.tar.gz"

echo ""
echo "=== Downloads complete ==="
echo "Sources: $(ls "$SRC" 2>/dev/null | wc -l) files in $SRC"
echo "Failed:  $FAILED (non-fatal — Alpine apk provides packages)"
ls -lh "$SRC" 2>/dev/null | head -5 || true
# Non-fatal: packages are installed via Alpine apk
if [ "$STRICT" = "1" ] && [ "$FAILED" -gt 0 ]; then
    echo "ERROR: STRICT mode — $FAILED downloads failed"
    exit 1
fi
exit 0
