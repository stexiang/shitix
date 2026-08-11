#!/bin/bash
# LFS 12.2 - Configure System
# Sets up /etc files, init script, symlinks, and runs ldconfig.
set -e

if [ -z "$LFS" ]; then
    echo "ERROR: \$LFS is not set"
    exit 1
fi

echo "=== Configuring LFS System ==="

# ============================================================
# Install config files from /build/config/
# ============================================================
echo "--- Installing config files ---"
cp -v /build/config/passwd     $LFS/etc/passwd
cp -v /build/config/group      $LFS/etc/group
cp -v /build/config/profile    $LFS/etc/profile
cp -v /build/config/hostname   $LFS/etc/hostname
cp -v /build/config/hosts      $LFS/etc/hosts
cp -v /build/config/fstab      $LFS/etc/fstab
cp -v /build/config/ld.so.conf $LFS/etc/ld.so.conf
cp -v /build/config/nsswitch.conf $LFS/etc/nsswitch.conf

# ============================================================
# Create /etc/shadow (locked root password — login with no password)
# ============================================================
cat > $LFS/etc/shadow << "EOF"
root::19967:0:99999:7:::
bin:*:19967:0:99999:7:::
daemon:*:19967:0:99999:7:::
nobody:*:19967:0:99999:7:::
EOF
chmod 600 $LFS/etc/shadow

# ============================================================
# Create /etc/shells
# ============================================================
cat > $LFS/etc/shells << "EOF"
/bin/bash
/bin/sh
EOF

# ============================================================
# Create /etc/inputrc (readline configuration)
# ============================================================
cat > $LFS/etc/inputrc << "EOF"
# /etc/inputrc - Readline initialization

# Allow 8-bit input
set input-meta on
set output-meta on
set convert-meta off

# Bell style
set bell-style none

# Key bindings
"\e[1~": beginning-of-line    # Home
"\e[4~": end-of-line          # End
"\e[5~": beginning-of-history # Page Up
"\e[6~": end-of-history       # Page Down
"\e[3~": delete-char          # Delete
"\e[2~": quoted-insert        # Insert

# Arrow keys in keypad mode
"\eOA": history-search-backward
"\eOB": history-search-forward

# Tab completion
set show-all-if-ambiguous on
set completion-ignore-case on
EOF

# ============================================================
# Create /etc/os-release
# ============================================================
cat > $LFS/etc/os-release << "EOF"
NAME="Shitix LFS"
VERSION="1.0 (LFS 12.2)"
ID=shitix
VERSION_ID=1.0
PRETTY_NAME="Shitix LFS 1.0"
HOME_URL="https://github.com/user/shitix"
EOF

# ============================================================
# Create /etc/lfs-release
# ============================================================
echo "12.2" > $LFS/etc/lfs-release

# ============================================================
# Create /etc/issue (login banner)
# ============================================================
cat > $LFS/etc/issue << "EOF"
Shitix LFS 1.0 \n \l

EOF

# ============================================================
# Create /sbin/init script
# ============================================================
echo "--- Creating /sbin/init ---"
cat > $LFS/sbin/init << 'INITEOF'
#!/bin/bash
# Shitix minimal init

# Mount virtual filesystems
mount -t proc     proc     /proc    2>/dev/null
mount -t sysfs    sysfs    /sys     2>/dev/null
mount -t devtmpfs devtmpfs /dev     2>/dev/null
mount -t tmpfs    tmpfs    /tmp     2>/dev/null
mount -t tmpfs    tmpfs    /var/tmp 2>/dev/null
mount -t tmpfs    tmpfs    /run     2>/dev/null

# Create device symlinks
ln -sf /proc/self/fd   /dev/fd    2>/dev/null
ln -sf /proc/self/fd/0 /dev/stdin 2>/dev/null
ln -sf /proc/self/fd/1 /dev/stdout 2>/dev/null
ln -sf /proc/self/fd/2 /dev/stderr 2>/dev/null

# Mount devpts for pseudo-terminals
mkdir -p /dev/pts /dev/shm
mount -t devpts devpts /dev/pts 2>/dev/null
mount -t tmpfs  tmpfs  /dev/shm 2>/dev/null

# Set hostname
if [ -f /etc/hostname ]; then
    hostname $(cat /etc/hostname)
fi

# Set environment
export PATH=/usr/bin:/usr/sbin:/bin:/sbin
export HOME=/root
export TERM=linux
export PS1='\u@shitix:\w\$ '
export LANG=C.UTF-8

# Load profile
[ -f /etc/profile ] && . /etc/profile

# Run ldconfig
ldconfig 2>/dev/null

# Print welcome
echo ""
echo "  _____ _     _ _   _      "
echo " / ____| |   (_) | (_)     "
echo "| (___ | |__  _| |_ ___  __"
echo " \___ \| '_ \| | __| \ \/ /"
echo " ____) | | | | | |_| |>  < "
echo "|_____/|_| |_|_|\__|_/_/\_\\"
echo ""
echo "Welcome to Shitix LFS!"
echo "Kernel: $(uname -r)"
echo ""

# Start a login shell
exec /bin/bash --login
INITEOF
chmod 755 $LFS/sbin/init

# Also create a simpler /init for direct kernel boot
ln -sfv sbin/init $LFS/init

# ============================================================
# Create essential symlinks
# ============================================================
echo "--- Creating symlinks ---"

# sh -> bash
ln -sfv bash $LFS/usr/bin/sh

# Ensure /usr/bin/env exists (Python, Perl scripts need it)
# It should already be there from coreutils, but make sure
if [ ! -f $LFS/usr/bin/env ] && [ -f $LFS/bin/env ]; then
    : # /bin is already a symlink to /usr/bin
fi

# ============================================================
# Create essential directories
# ============================================================
mkdir -pv $LFS/root
chmod 0750 $LFS/root

# Create root's .bashrc
cat > $LFS/root/.bashrc << "EOF"
# ~/.bashrc
export PS1='\[\e[1;31m\]\u@shitix\[\e[0m\]:\[\e[1;34m\]\w\[\e[0m\]\$ '
alias ls='ls --color=auto'
alias ll='ls -la'
alias grep='grep --color=auto'
EOF

cat > $LFS/root/.bash_profile << "EOF"
# ~/.bash_profile
[ -f /etc/profile ] && . /etc/profile
[ -f ~/.bashrc ] && . ~/.bashrc
EOF

# ============================================================
# Run ldconfig to generate ld.so.cache
# ============================================================
echo "--- Running ldconfig ---"
# We can't run the target's ldconfig directly (it's for the target system).
# Instead, use the host's ldconfig with the target's config.
# This generates the ld.so.cache for the target rootfs.
if [ -x $LFS/usr/sbin/ldconfig ]; then
    # If we can run it (same arch), do so with chroot-like approach
    LD_LIBRARY_PATH=$LFS/usr/lib $LFS/usr/sbin/ldconfig -r $LFS 2>/dev/null || \
    ldconfig -f $LFS/etc/ld.so.conf -C $LFS/etc/ld.so.cache 2>/dev/null || \
    echo "Warning: ldconfig failed, ld.so.cache will be generated on first boot"
else
    echo "Warning: Cannot run target ldconfig, cache will be generated on first boot"
fi

# ============================================================
# Create /var/log files
# ============================================================
touch $LFS/var/log/{btmp,lastlog,faillog,wtmp}
chmod -v 664  $LFS/var/log/lastlog
chmod -v 600  $LFS/var/log/btmp

# ============================================================
# Clean up unnecessary files to reduce image size
# ============================================================
echo "--- Cleaning up ---"

# Remove documentation to save space (optional)
# rm -rf $LFS/usr/share/doc/*
# rm -rf $LFS/usr/share/info/*
# rm -rf $LFS/usr/share/man/*

# Remove .la files (libtool archives — not needed at runtime)
find $LFS/usr/lib -name '*.la' -delete 2>/dev/null || true

# Remove static libraries (optional — saves significant space)
# find $LFS/usr/lib -name '*.a' -delete 2>/dev/null || true

echo ""
echo "=== System configuration complete ==="
echo ""
echo "Summary:"
echo "  Hostname:  $(cat $LFS/etc/hostname)"
echo "  Init:      $LFS/sbin/init (shell script)"
echo "  Shell:     /bin/bash"
echo "  Root home: /root (no password)"
echo ""
du -sh $LFS/
