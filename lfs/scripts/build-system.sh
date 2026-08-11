#!/bin/bash
# LFS 12.2 - Build Final System Packages
# All packages built with host compiler, installed to $LFS with DESTDIR
set -e

if [ -z "$LFS" ]; then
    echo "ERROR: \$LFS is not set"
    exit 1
fi

SRC=/build/sources
NPROC=$(nproc)

echo "=== Building LFS System Packages (${NPROC} cores) ==="

extract() {
    local tarball="$1"
    local dir="${2:-}"
    cd $SRC
    echo ""
    echo "--- Extracting $tarball ---"
    tar xf "$tarball"
    if [ -n "$dir" ]; then
        cd "$dir"
    else
        local extracted=$(tar tf "$tarball" | head -1 | cut -d/ -f1)
        cd "$extracted"
    fi
}

cleanup() {
    local dir="$1"
    cd $SRC
    rm -rf "$dir"
}

PKG=0
total=52
step() {
    PKG=$((PKG + 1))
    echo ""
    echo "=========================================="
    echo "=== [$PKG/$total] $1"
    echo "=========================================="
}

# ============================================================
step "Man-pages 6.9.1"
# ============================================================
extract man-pages-6.9.1.tar.xz
rm -v man3/crypt*
make DESTDIR=$LFS prefix=/usr install
cleanup man-pages-6.9.1

# ============================================================
step "Iana-Etc 20240806"
# ============================================================
extract iana-etc-20240806.tar.gz
cp -v services protocols $LFS/etc/
cleanup iana-etc-20240806

# ============================================================
step "Bzip2 1.0.8"
# ============================================================
extract bzip2-1.0.8.tar.gz

# Ensure shared library is built
sed -i 's@\(ln -s -f \)$(PREFIX)/bin/@\1@' Makefile
sed -i "s@(PREFIX)/man@(PREFIX)/share/man@g" Makefile

make -f Makefile-libbz2_so -j$NPROC
make clean
make -j$NPROC

make PREFIX=$LFS/usr install
cp -av libbz2.so* $LFS/usr/lib/
ln -sfv libbz2.so.1.0.8 $LFS/usr/lib/libbz2.so
cp -v bzip2-shared $LFS/usr/bin/bzip2
ln -sfv bzip2 $LFS/usr/bin/bunzip2
ln -sfv bzip2 $LFS/usr/bin/bzcat
rm -fv $LFS/usr/lib/libbz2.a
cleanup bzip2-1.0.8

# ============================================================
step "Xz 5.6.2"
# ============================================================
extract xz-5.6.2.tar.xz
./configure --prefix=/usr \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
cleanup xz-5.6.2

# ============================================================
step "Lz4 1.10.0"
# ============================================================
extract lz4-1.10.0.tar.gz lz4-1.10.0
make -j$NPROC BUILD_STATIC=no PREFIX=/usr
make BUILD_STATIC=no PREFIX=/usr DESTDIR=$LFS install
cleanup lz4-1.10.0

# ============================================================
step "Zstd 1.5.6"
# ============================================================
extract zstd-1.5.6.tar.gz
make -j$NPROC prefix=/usr
make prefix=/usr DESTDIR=$LFS install
rm -fv $LFS/usr/lib/libzstd.a
cleanup zstd-1.5.6

# ============================================================
step "File 5.45"
# ============================================================
extract file-5.45.tar.gz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup file-5.45

# ============================================================
step "Readline 8.2.13"
# ============================================================
extract readline-8.2.13.tar.gz
sed -i '/MV.*telerator/d' shlib/Makefile.in
sed -i 's|-O2|& -g|' configure
./configure --prefix=/usr \
    --disable-static \
    --with-curses
make -j$NPROC SHLIB_LIBS="-lncursesw"
make SHLIB_LIBS="-lncursesw" DESTDIR=$LFS install
cleanup readline-8.2.13

# ============================================================
step "M4 1.4.19"
# ============================================================
extract m4-1.4.19.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup m4-1.4.19

# ============================================================
step "Bc 6.7.6"
# ============================================================
extract bc-6.7.6.tar.xz
CC=gcc ./configure --prefix=/usr -G -O3 -r
make -j$NPROC
make DESTDIR=$LFS install
cleanup bc-6.7.6

# ============================================================
step "Flex 2.6.4"
# ============================================================
extract flex-2.6.4.tar.gz
./configure --prefix=/usr \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
ln -sfv flex $LFS/usr/bin/lex
cleanup flex-2.6.4

# ============================================================
step "Tcl 8.6.14"
# ============================================================
extract tcl8.6.14-src.tar.gz tcl8.6.14
SRCDIR=$(pwd)
cd unix
./configure --prefix=/usr \
    --mandir=/usr/share/man \
    --enable-64bit
make -j$NPROC
make DESTDIR=$LFS install
chmod -v u+w $LFS/usr/lib/libtcl8.6.so 2>/dev/null || true
make DESTDIR=$LFS install-private-headers
ln -sfv tclsh8.6 $LFS/usr/bin/tclsh
cd $SRC
cleanup tcl8.6.14

# ============================================================
step "Expect 5.45.4"
# ============================================================
extract expect5.45.4.tar.gz expect5.45.4
python3 -c 'import os; data=open("configure").read().replace("/usr/local/bin","$LFS/usr/bin"); open("configure","w").write(data)' 2>/dev/null || true
./configure --prefix=/usr \
    --with-tcl=/usr/lib \
    --enable-shared \
    --mandir=/usr/share/man \
    --with-tclinclude=/usr/include
make -j$NPROC
make DESTDIR=$LFS install
ln -sfv expect5.45.4/libexpect5.45.4.so $LFS/usr/lib/
cleanup expect5.45.4

# ============================================================
step "DejaGNU 1.6.3"
# ============================================================
extract dejagnu-1.6.3.tar.gz
mkdir -v build
cd build
../configure --prefix=/usr
make DESTDIR=$LFS install
cd $SRC
cleanup dejagnu-1.6.3

# ============================================================
step "Pkgconf 2.3.0"
# ============================================================
extract pkgconf-2.3.0.tar.xz
./configure --prefix=/usr \
    --disable-static \
    --with-pkg-config-dir=/usr/lib/pkgconfig:/usr/share/pkgconfig
make -j$NPROC
make DESTDIR=$LFS install
ln -sfv pkgconf $LFS/usr/bin/pkg-config
cleanup pkgconf-2.3.0

# ============================================================
step "Ncurses 6.5"
# ============================================================
extract ncurses-6.5.tar.gz
./configure --prefix=/usr \
    --mandir=/usr/share/man \
    --with-shared \
    --without-debug \
    --without-normal \
    --with-cxx-shared \
    --enable-pc-files \
    --with-pkg-config-libdir=/usr/lib/pkgconfig \
    --enable-widec
make -j$NPROC
make DESTDIR=$LFS install

# Create non-wide-character compatibility symlinks
for lib in ncurses form panel menu; do
    ln -sfv lib${lib}w.so $LFS/usr/lib/lib${lib}.so
    ln -sfv ${lib}w.pc $LFS/usr/lib/pkgconfig/${lib}.pc
done
ln -sfv libncursesw.so $LFS/usr/lib/libcurses.so

cleanup ncurses-6.5

# ============================================================
step "Sed 4.9"
# ============================================================
extract sed-4.9.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup sed-4.9

# ============================================================
step "Psmisc 23.7"
# ============================================================
extract psmisc-23.7.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup psmisc-23.7

# ============================================================
step "Gettext 0.22.5"
# ============================================================
extract gettext-0.22.5.tar.xz
./configure --prefix=/usr \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
chmod -v 0755 $LFS/usr/lib/preloadable_libintl.so 2>/dev/null || true
cleanup gettext-0.22.5

# ============================================================
step "Bison 3.8.2"
# ============================================================
extract bison-3.8.2.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup bison-3.8.2

# ============================================================
step "Grep 3.11"
# ============================================================
extract grep-3.11.tar.xz
sed -i "s/echo/#echo/" src/init.c 2>/dev/null || true
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup grep-3.11

# ============================================================
step "Bash 5.2.32"
# ============================================================
extract bash-5.2.32.tar.gz
./configure --prefix=/usr \
    --without-bash-malloc \
    --with-installed-readline
make -j$NPROC
make DESTDIR=$LFS install
cleanup bash-5.2.32

# ============================================================
step "Libtool 2.4.7"
# ============================================================
extract libtool-2.4.7.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
rm -fv $LFS/usr/lib/libltdl.a
cleanup libtool-2.4.7

# ============================================================
step "GDBM 1.24"
# ============================================================
extract gdbm-1.24.tar.gz
./configure --prefix=/usr \
    --disable-static \
    --enable-libgdbm-compat
make -j$NPROC
make DESTDIR=$LFS install
cleanup gdbm-1.24

# ============================================================
step "Gperf 3.1"
# ============================================================
extract gperf-3.1.tar.gz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup gperf-3.1

# ============================================================
step "Expat 2.6.2"
# ============================================================
extract expat-2.6.2.tar.xz
./configure --prefix=/usr \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
cleanup expat-2.6.2

# ============================================================
step "Inetutils 2.5"
# ============================================================
extract inetutils-2.5.tar.xz
./configure --prefix=/usr \
    --bindir=/usr/bin \
    --localstatedir=/var \
    --disable-logger \
    --disable-whois \
    --disable-rcp \
    --disable-rexec \
    --disable-rlogin \
    --disable-rsh \
    --disable-servers
make -j$NPROC
make DESTDIR=$LFS install
cleanup inetutils-2.5

# ============================================================
step "Less 661"
# ============================================================
extract less-661.tar.gz
./configure --prefix=/usr \
    --sysconfdir=/etc
make -j$NPROC
make DESTDIR=$LFS install
cleanup less-661

# ============================================================
step "Perl 5.40.0"
# ============================================================
extract perl-5.40.0.tar.xz

export BUILD_ZLIB=False
export BUILD_BZIP2=0

sh Configure -des \
    -Dprefix=/usr \
    -Dvendorprefix=/usr \
    -Dprivlib=/usr/lib/perl5/5.40/core_perl \
    -Darchlib=/usr/lib/perl5/5.40/core_perl \
    -Dsitelib=/usr/lib/perl5/5.40/site_perl \
    -Dsitearch=/usr/lib/perl5/5.40/site_perl \
    -Dvendorlib=/usr/lib/perl5/5.40/vendor_perl \
    -Dvendorarch=/usr/lib/perl5/5.40/vendor_perl \
    -Dman1dir=/usr/share/man/man1 \
    -Dman3dir=/usr/share/man/man3 \
    -Dpager="/usr/bin/less -isR" \
    -Duseshrplib \
    -Dusethreads

make -j$NPROC
make DESTDIR=$LFS install
unset BUILD_ZLIB BUILD_BZIP2
cleanup perl-5.40.0

# ============================================================
step "XML::Parser 2.47"
# ============================================================
extract XML-Parser-2.47.tar.gz
perl Makefile.PL
make -j$NPROC
make DESTDIR=$LFS install
cleanup XML-Parser-2.47

# ============================================================
step "Intltool 0.51.0"
# ============================================================
extract intltool-0.51.0.tar.gz
sed -i 's:\\\${:\\\$\\{:' intltool-update.in 2>/dev/null || true
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup intltool-0.51.0

# ============================================================
step "Autoconf 2.72"
# ============================================================
extract autoconf-2.72.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup autoconf-2.72

# ============================================================
step "Automake 1.17"
# ============================================================
extract automake-1.17.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup automake-1.17

# ============================================================
step "OpenSSL 3.3.1"
# ============================================================
extract openssl-3.3.1.tar.gz
./config --prefix=/usr \
    --openssldir=/etc/ssl \
    --libdir=lib \
    shared \
    zlib-dynamic
make -j$NPROC
sed -i '/INSTALL_LIBS/s/libcrypto.a libssl.a//' Makefile
make DESTDIR=$LFS MANSUFFIX=ssl install
cleanup openssl-3.3.1

# ============================================================
step "Libffi 3.4.6"
# ============================================================
extract libffi-3.4.6.tar.gz
./configure --prefix=/usr \
    --disable-static \
    --with-gcc-arch=native
make -j$NPROC
make DESTDIR=$LFS install
cleanup libffi-3.4.6

# ============================================================
step "Python 3.12.5"
# ============================================================
extract Python-3.12.5.tar.xz
./configure --prefix=/usr \
    --enable-shared \
    --with-system-expat \
    --without-ensurepip
make -j$NPROC
make DESTDIR=$LFS install
cleanup Python-3.12.5

# ============================================================
step "Texinfo 7.1"
# ============================================================
extract texinfo-7.1.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup texinfo-7.1

# ============================================================
step "Kmod 33"
# ============================================================
extract kmod-33.tar.xz
./configure --prefix=/usr \
    --sysconfdir=/etc \
    --with-openssl \
    --with-xz \
    --with-zstd \
    --with-zlib
make -j$NPROC
make DESTDIR=$LFS install

# Create symlinks for module utilities
for target in depmod insmod modinfo modprobe rmmod; do
    ln -sfv ../bin/kmod $LFS/usr/sbin/$target
done
ln -sfv kmod $LFS/usr/bin/lsmod
cleanup kmod-33

# ============================================================
step "Libcap 2.70"
# ============================================================
extract libcap-2.70.tar.xz
sed -i '/install -m.*STA/d' libcap/Makefile
make -j$NPROC prefix=/usr lib=lib
make prefix=/usr lib=lib DESTDIR=$LFS install
cleanup libcap-2.70

# ============================================================
step "Attr 2.5.2"
# ============================================================
extract attr-2.5.2.tar.xz
./configure --prefix=/usr \
    --disable-static \
    --sysconfdir=/etc
make -j$NPROC
make DESTDIR=$LFS install
cleanup attr-2.5.2

# ============================================================
step "Acl 2.3.2"
# ============================================================
extract acl-2.3.2.tar.xz
./configure --prefix=/usr \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
cleanup acl-2.3.2

# ============================================================
step "Libxcrypt 4.4.36"
# ============================================================
extract libxcrypt-4.4.36.tar.xz
./configure --prefix=/usr \
    --enable-hashes=strong,glibc \
    --enable-obsolete-api=no \
    --disable-static \
    --disable-failure-tokens
make -j$NPROC
make DESTDIR=$LFS install
cleanup libxcrypt-4.4.36

# ============================================================
step "Shadow 4.16.0"
# ============================================================
extract shadow-4.16.0.tar.xz
# Disable installation of groups program (coreutils provides it)
sed -i 's/groups$(EXEEXT) //' src/Makefile.in
find man -name Makefile.in -exec sed -i 's/groups\.1 / /' {} \; 2>/dev/null || true

# Use crypt() from libxcrypt
sed -e 's:#ENCRYPT_METHOD DES:ENCRYPT_METHOD YESCRYPT:' \
    -e 's:/var/spool/mail:/var/mail:' \
    -e '/PATH=/{s@/sbin:@@;s@/bin:@@}' \
    -i etc/login.defs

./configure --sysconfdir=/etc \
    --disable-static \
    --with-{b,yes}crypt \
    --without-libbsd \
    --with-group-name-max-length=32
make -j$NPROC
make DESTDIR=$LFS exec_prefix=/usr install
mkdir -pv $LFS/etc/default
cleanup shadow-4.16.0

# ============================================================
step "Coreutils 9.5"
# ============================================================
extract coreutils-9.5.tar.xz
patch -Np1 -i /build/patches/coreutils-9.5-i18n-2.patch 2>/dev/null || true
autoreconf -fiv 2>/dev/null || true
FORCE_UNSAFE_CONFIGURE=1 ./configure \
    --prefix=/usr \
    --enable-no-install-program=kill,uptime
make -j$NPROC
make DESTDIR=$LFS install

# Move programs to standard locations
mkdir -pv $LFS/usr/sbin
mv -v $LFS/usr/bin/chroot $LFS/usr/sbin/ 2>/dev/null || true
cleanup coreutils-9.5

# ============================================================
step "Check 0.15.2"
# ============================================================
extract check-0.15.2.tar.gz
./configure --prefix=/usr \
    --disable-static
make -j$NPROC
make DESTDIR=$LFS install
cleanup check-0.15.2

# ============================================================
step "Diffutils 3.10"
# ============================================================
extract diffutils-3.10.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup diffutils-3.10

# ============================================================
step "Gawk 5.3.0"
# ============================================================
extract gawk-5.3.0.tar.xz
sed -i 's/extras//' Makefile.in
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS LN='ln -f' install
cleanup gawk-5.3.0

# ============================================================
step "Findutils 4.10.0"
# ============================================================
extract findutils-4.10.0.tar.xz
./configure --prefix=/usr \
    --localstatedir=/var/lib/locate
make -j$NPROC
make DESTDIR=$LFS install
cleanup findutils-4.10.0

# ============================================================
step "Groff 1.23.0"
# ============================================================
extract groff-1.23.0.tar.gz
PAGE=letter ./configure --prefix=/usr
make -j1
make DESTDIR=$LFS install
cleanup groff-1.23.0

# ============================================================
step "Gzip 1.13"
# ============================================================
extract gzip-1.13.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup gzip-1.13

# ============================================================
step "IPRoute2 6.10.0"
# ============================================================
extract iproute2-6.10.0.tar.xz
sed -i /ARPD/d Makefile
rm -fv man/man8/arpd.8
make -j$NPROC NETNS_RUN_DIR=/run/netns
make SBINDIR=/usr/sbin DESTDIR=$LFS install
cleanup iproute2-6.10.0

# ============================================================
step "Kbd 2.6.4"
# ============================================================
extract kbd-2.6.4.tar.xz
sed -i '/RESIZECONS_PROGS/s/yes/no/' configure
sed -i 's/resizecons.8 //' docs/man/man8/Makefile.in
./configure --prefix=/usr \
    --disable-vlock
make -j$NPROC
make DESTDIR=$LFS install
cleanup kbd-2.6.4

# ============================================================
step "Libpipeline 1.5.7"
# ============================================================
extract libpipeline-1.5.7.tar.gz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup libpipeline-1.5.7

# ============================================================
step "Make 4.4.1"
# ============================================================
extract make-4.4.1.tar.gz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup make-4.4.1

# ============================================================
step "Patch 2.7.6"
# ============================================================
extract patch-2.7.6.tar.xz
./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup patch-2.7.6

# ============================================================
step "Tar 1.35"
# ============================================================
extract tar-1.35.tar.xz
FORCE_UNSAFE_CONFIGURE=1 ./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install
cleanup tar-1.35

# ============================================================
step "Man-DB 2.12.1"
# ============================================================
extract man-db-2.12.1.tar.xz
./configure --prefix=/usr \
    --sysconfdir=/etc \
    --disable-setuid \
    --enable-cache-owner=bin \
    --with-browser=/usr/bin/lynx \
    --with-vgrind=/usr/bin/vgrind \
    --with-grap=/usr/bin/grap
make -j$NPROC
make DESTDIR=$LFS install
cleanup man-db-2.12.1

# ============================================================
step "Procps-ng 4.0.4"
# ============================================================
extract procps-ng-4.0.4.tar.xz
./configure --prefix=/usr \
    --disable-static \
    --disable-kill \
    --with-systemd=no
make -j$NPROC
make DESTDIR=$LFS install
cleanup procps-ng-4.0.4

# ============================================================
step "Util-linux 2.40.2"
# ============================================================
extract util-linux-2.40.2.tar.xz
./configure --bindir=/usr/bin \
    --libdir=/usr/lib \
    --runstatedir=/run \
    --sbindir=/usr/sbin \
    --disable-chfn-chsh \
    --disable-login \
    --disable-nologin \
    --disable-su \
    --disable-setpriv \
    --disable-runuser \
    --disable-pylibmount \
    --disable-liblastlog2 \
    --disable-static \
    --without-python \
    --without-systemd \
    --without-systemdsystemunitdir \
    ADJTIME_PATH=/var/lib/hwclock/adjtime
make -j$NPROC
make DESTDIR=$LFS install
cleanup util-linux-2.40.2

# ============================================================
step "E2fsprogs 1.47.1"
# ============================================================
extract e2fsprogs-1.47.1.tar.gz
mkdir -v build
cd build
../configure --prefix=/usr \
    --sysconfdir=/etc \
    --enable-elf-shlibs \
    --disable-libblkid \
    --disable-libuuid \
    --disable-uuidd \
    --disable-fsck
make -j$NPROC
make DESTDIR=$LFS install
rm -fv $LFS/usr/lib/{libcom_err,libe2p,libext2fs,libss}.a
cd $SRC
cleanup e2fsprogs-1.47.1

# ============================================================
step "Vim 9.1.0660"
# ============================================================
extract vim-9.1.0660.tar.gz vim-9.1.0660

echo '#define SYS_VIMRC_FILE "/etc/vimrc"' >> src/feature.h

./configure --prefix=/usr
make -j$NPROC
make DESTDIR=$LFS install

# Create vi symlink
ln -sfv vim $LFS/usr/bin/vi

# Create default vimrc
cat > $LFS/etc/vimrc << "VIMRC"
" Begin /etc/vimrc
set nocompatible
set backspace=2
set mouse=
syntax on
if (&term == "xterm") || (&term == "putty")
  set background=dark
endif
" End /etc/vimrc
VIMRC

cleanup vim-9.1.0660

# ============================================================
# Strip debug symbols from all binaries and libraries
# ============================================================
echo ""
echo "=== Stripping debug symbols ==="
find $LFS/usr/lib -type f -name '*.a' -exec strip --strip-debug {} \; 2>/dev/null || true
find $LFS/usr/lib -type f -name '*.so*' -exec strip --strip-unneeded {} \; 2>/dev/null || true
find $LFS/usr/bin $LFS/usr/sbin -type f -exec strip --strip-unneeded {} \; 2>/dev/null || true

# Clean up .la files (libtool archives)
find $LFS/usr/lib -name '*.la' -delete 2>/dev/null || true

echo ""
echo "=== System build complete ==="
echo "Total packages built: $PKG"
du -sh $LFS/
