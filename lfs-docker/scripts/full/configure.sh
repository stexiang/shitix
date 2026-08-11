#!/bin/bash
# Final system configuration for LFS
set -e
DEST=/lfs

echo "=== Configuring LFS System ==="

# Create essential directories
mkdir -p $DEST/{dev,proc,sys,run,tmp,var/{log,lock,run,tmp},root,home}
chmod 1777 $DEST/tmp $DEST/var/tmp

# Device nodes
mknod -m 666 $DEST/dev/null c 1 3 2>/dev/null || true
mknod -m 666 $DEST/dev/zero c 1 5 2>/dev/null || true
mknod -m 666 $DEST/dev/tty c 5 0 2>/dev/null || true
mknod -m 600 $DEST/dev/console c 5 1 2>/dev/null || true
ln -sf /proc/self/fd/0 $DEST/dev/stdin 2>/dev/null || true
ln -sf /proc/self/fd/1 $DEST/dev/stdout 2>/dev/null || true
ln -sf /proc/self/fd/2 $DEST/dev/stderr 2>/dev/null || true

# /etc/passwd
cat > $DEST/etc/passwd << 'EOF'
root:x:0:0:root:/root:/bin/bash
bin:x:1:1:bin:/bin:/bin/false
daemon:x:2:2:daemon:/sbin:/bin/false
nobody:x:99:99:nobody:/:/bin/false
EOF

# /etc/group
cat > $DEST/etc/group << 'EOF'
root:x:0:
bin:x:1:
daemon:x:2:
sys:x:3:
adm:x:4:
tty:x:5:
disk:x:6:
lp:x:7:
mail:x:8:
nogroup:x:99:
nobody:x:99:
EOF

# /etc/hosts
cat > $DEST/etc/hosts << 'EOF'
127.0.0.1 localhost
::1 localhost
EOF

# /etc/hostname
echo "shitix" > $DEST/etc/hostname

# /etc/fstab
cat > $DEST/etc/fstab << 'EOF'
proc  /proc  proc  defaults  0 0
tmpfs /tmp   tmpfs defaults  0 0
EOF

# /etc/profile
cat > $DEST/etc/profile << 'EOF'
export PATH=/bin:/usr/bin:/sbin:/usr/sbin
export HOME=/root
export TERM=linux
export PS1='[\u@\h \w]\$ '
export LANG=C.UTF-8
EOF

# /etc/inittab (SysVinit)
cat > $DEST/etc/inittab << 'EOF'
id:3:initdefault:
si::sysinit:/etc/rc.d/init.d/rc sysinit
l0:0:wait:/etc/rc.d/init.d/rc 0
l1:S1:wait:/etc/rc.d/init.d/rc 1
l2:2:wait:/etc/rc.d/init.d/rc 2
l3:3:wait:/etc/rc.d/init.d/rc 3
l4:4:wait:/etc/rc.d/init.d/rc 4
l5:5:wait:/etc/rc.d/init.d/rc 5
l6:6:wait:/etc/rc.d/init.d/rc 6
1:2345:respawn:/sbin/agetty tty1 9600
c1:12345:respawn:/sbin/agetty -L ttyS0 115200 linux
EOF

# /etc/issue
echo "shitix LFS 1.0" > $DEST/etc/issue

# /sbin/init symlink
ln -sf /sbin/init $DEST/init 2>/dev/null || true

# Create minimal init script for our kernel
cat > $DEST/init << 'INIT'
#!/bin/bash
echo "LFS: System boot"
mount -t proc proc /proc
mount -t tmpfs tmpfs /tmp
echo "LFS: /proc /tmp mounted"
echo "LFS_BOOT_SUCCESS"
exec /bin/bash --login
INIT
chmod +x $DEST/init

echo "Configuration complete"
