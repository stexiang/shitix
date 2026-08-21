#!/usr/bin/env bash
#
# `make install` — install the built shitix kernel into the current LFS system.
#
# Modeled on the LFS book's kernel "make install" step: copy the kernel image,
# System.map, and the config into /boot (no modules — shitix is monolithic).
#
# Install destination is resolved in this order:
#   1. CONFIG_INSTALL_ROOT   (set in .config, or via `make install CONFIG_INSTALL_ROOT=...`)
#   2. $DESTDIR              (standard GNU convention)
#   3. $LFS                  (LFS book convention when building outside the chroot)
#   4. /mnt/lfs              (common LFS mount point)
#   5. ""  -> /              (already inside the chroot, install to the live /boot)
#
# Example (outside chroot):   LFS=/mnt/lfs make install
# Example (inside chroot):    make install
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# ---- configuration ---------------------------------------------------------
# The Makefile exports these; when running the script directly, read the files.
VERSION="${CONFIG_VERSION:-}"
INSTALL_ROOT_CFG="${CONFIG_INSTALL_ROOT:-}"
BOOTDIR_CFG="${CONFIG_INSTALL_BOOTDIR:-}"

if [[ -z "$VERSION" && -z "$BOOTDIR_CFG" ]]; then
    # Direct invocation fallback: merge defaults + .config
    # shellcheck disable=SC1091
    source config/defaults
    [[ -f .config ]] && source .config
    VERSION="${CONFIG_VERSION:-0.1.0}"
    INSTALL_ROOT_CFG="${CONFIG_INSTALL_ROOT:-}"
    BOOTDIR_CFG="${CONFIG_INSTALL_BOOTDIR:-/boot}"
fi

VERSION="${VERSION:-0.1.0}"
BOOTDIR_CFG="${BOOTDIR_CFG:-/boot}"

# root filesystem image (for the combined-image / boot-sector step below).
# Set in .config as CONFIG_ROOTIMG; the Makefile exports it on `make install`.
ROOTIMG="${CONFIG_ROOTIMG:-}"

# ---- resolve install destination ------------------------------------------
DEST=""
if [[ -n "$INSTALL_ROOT_CFG" ]]; then
    DEST="$INSTALL_ROOT_CFG"
elif [[ -n "${DESTDIR:-}" ]]; then
    DEST="$DESTDIR"
elif [[ -n "${LFS:-}" ]]; then
    DEST="$LFS"
elif [[ -d /mnt/lfs ]]; then
    DEST="/mnt/lfs"
fi

BOOTDIR="${BOOTDIR_CFG#/}"          # strip leading slash for clean joining
DEST_BOOT="$DEST/$BOOTDIR"          # "" + "/boot" -> "/boot" (chroot case)

IMG="target/boot/shitix.img"
ELF="target/boot/system.elf"
SYSMAP="target/boot/System.map"

step() { printf '\033[1;34m[install]\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

# ---- preflight -------------------------------------------------------------
[[ -f "$IMG" ]] || die "kernel image not found: $IMG (run `make` first)"
[[ -f "$ELF" ]] || die "kernel ELF not found: $ELF (run `make` first)"
if [[ ! -f .config ]]; then
    cp config/defaults .config
    echo "  .config missing — generated from config/defaults"
fi

step "installing into $DEST_BOOT"
if ! mkdir -p "$DEST_BOOT" 2>/dev/null || [[ ! -w "$DEST_BOOT" ]]; then
    die "cannot write to $DEST_BOOT (permission denied).
  hints:
    - install into an LFS root:  LFS=/mnt/lfs make install   (or CONFIG_INSTALL_ROOT)
    - inside the chroot:         run as root (make install)
    - otherwise:                 sudo make install"
fi

# ---- generate System.map ---------------------------------------------------
if command -v nm >/dev/null 2>&1; then
    step "generating System.map"
    nm -n "$ELF" > "$SYSMAP" 2>/dev/null || true
fi

# ---- install ---------------------------------------------------------------

install -m 644 "$IMG"       "$DEST_BOOT/vmlinuz-${VERSION}-shitix"
[[ -f "$SYSMAP" ]] && install -m 644 "$SYSMAP" "$DEST_BOOT/System.map-${VERSION}-shitix"
install -m 644 "$ELF"       "$DEST_BOOT/shitix-${VERSION}.elf"
install -m 644 .config      "$DEST_BOOT/config-${VERSION}-shitix"

echo ""
echo "Installed into $DEST_BOOT:"
echo "  vmlinuz-${VERSION}-shitix       (bootable disk image)"
echo "  System.map-${VERSION}-shitix    (symbol map)"
echo "  shitix-${VERSION}.elf           (ELF, for GDB)"
echo "  config-${VERSION}-shitix        (kernel configuration)"

# ---- combined bootable image (auto-add boot sector) --------------------------
#
# shitix boots via its own 512-byte bootsect at sector 0 (no GRUB/LILO), so a
# plain LFS rootfs image has no boot sector and SeaBIOS reports
# "not a bootable disk". Detect that (sector 0 must end in 0xAA55) and, when an
# image file is available, prepend the kernel image to a copy of the rootfs:
# the result is a single self-bootable disk (kernel at LBA 0..1MB, rootfs at
# 1MB+; the kernel auto-mounts the rootfs at the 1MB offset).
step "checking boot sector / combined image"

# Fall back to the file backing the install destination (loop mount) when
# CONFIG_ROOTIMG isn't set.
if [[ -z "$ROOTIMG" && -n "$DEST" ]] && command -v findmnt >/dev/null 2>&1; then
    src="$(findmnt -no SOURCE "$DEST" 2>/dev/null || true)"
    case "$src" in
        /dev/loop*)
            back="$(losetup -n -O BACK-FILE "$src" 2>/dev/null || true)"
            [[ -f "$back" ]] && ROOTIMG="$back"
            ;;
        *)
            [[ -f "$src" ]] && ROOTIMG="$src"
            ;;
    esac
fi

COMBINED="target/boot/shitix-lfs-combined.img"

if [[ -z "$ROOTIMG" || ! -f "$ROOTIMG" ]]; then
    echo "  no rootfs image found — skipped combined image (boot dual-drive instead)"
elif [[ "$(od -An -tx1 -j510 -N2 "$ROOTIMG" 2>/dev/null | tr -d ' \n')" == "55aa" ]]; then
    echo "  $ROOTIMG already has a boot sector (0xAA55) — nothing to add"
else
    echo "  no boot sector on $ROOTIMG — building combined image"
    cp "$IMG" "$COMBINED"
    PAD=$(( (($(stat -c%s "$IMG") + 1048575) / 1048576) * 1048576 ))
    truncate -s "$PAD" "$COMBINED"
    cat "$ROOTIMG" >> "$COMBINED"
    echo "  combined image: $COMBINED ($(du -h "$COMBINED" | cut -f1))"
fi

echo ""
echo "Boot it:"
echo "  # dual-drive (kernel + rootfs on 2 drives):"
echo "  qemu-system-x86_64 \\"
echo "    -drive format=raw,file=$IMG,if=ide \\"
echo "    -drive format=raw,file=${ROOTIMG:-<lfs-rootfs.img>},if=ide \\"
echo "    -m 256M -no-reboot -no-shutdown"
if [[ -f "$COMBINED" ]]; then
    echo "  # single-drive (combined, auto-generated):"
    echo "  qemu-system-x86_64 \\"
    echo "    -drive format=raw,file=$COMBINED,if=ide \\"
    echo "    -m 256M -no-reboot -no-shutdown"
fi
