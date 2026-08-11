# LFS rootfs for shitix kernel using Alpine (musl libc)
FROM alpine:3.19 AS builder

# Install all standard LFS utilities (all musl-linked)
RUN apk add --no-cache \
    bash coreutils binutils grep sed gawk \
    findutils diffutils patch tar gzip xz bzip2 \
    make m4 bison flex file \
    less ncurses ncurses-terminfo readline \
    musl musl-utils libcap \
    procps util-linux psmisc shadow \
    gcc libgcc libstdc++

# Set up essential structure
RUN ln -sf /bin/bash /bin/sh && \
    mkdir -p /etc /dev /proc /tmp /run /root /var/log /var/tmp && \
    chmod 1777 /tmp

# /etc config files
RUN echo "root:x:0:0:root:/root:/bin/bash" > /etc/passwd && \
    echo "root:x:0:" > /etc/group && \
    echo "root::0:::::" > /etc/shadow && \
    echo "shitix" > /etc/hostname && \
    printf 'export PATH=/bin:/usr/bin:/sbin:/usr/sbin\nexport HOME=/root\nexport TERM=linux\nexport PS1='"'"'\\w \\$ '"'"'\n' > /etc/profile

# Remove build tools we don't need at runtime
RUN rm -rf /usr/include /usr/share/man /usr/share/doc /usr/share/info \
    /usr/lib/pkgconfig /var/cache/apk 2>/dev/null || true

# Strip binaries to save space
RUN find /bin /usr/bin /sbin /usr/sbin /lib -type f -executable \
    -exec strip --strip-unneeded {} \; 2>/dev/null || true

# Flat copy: the rootfs IS the image
FROM scratch
COPY --from=builder / /
