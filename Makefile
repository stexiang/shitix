# shitix — Rust x86_64 rewrite of Linux 1.0.9
#
# Wraps scripts/build.sh / scripts/test.sh and adds a kernel-config workflow
# (terminal GUI) plus an LFS-style `make install`.
#
# Common targets:
#   make             build the bootable image  -> target/boot/shitix.img
#   make test        build + headless QEMU boot test
#   make run         build + interactive QEMU (graphical window)
#   make debug       build + QEMU paused, waiting for GDB on :1234
#   make config      interactive configuration in a terminal GUI
#                    (aliases: xconfig, gui, menuconfig)
#   make menuconfig  interactive configuration in the terminal (whiptail)
#   make oldconfig   non-interactively merge new options from config/defaults
#   make defconfig   reset .config to config/defaults
#   make install     build + install the kernel into the LFS system (chroot)
#   make clean       remove build artifacts (cargo clean + target/boot)
#   make mrproper    clean + delete .config
#   make help        this help

SHELL := /bin/bash
ROOT  := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))

# ---------------------------------------------------------------------------
# Configuration.  Load defaults first, then the user's .config (overrides).
# Command-line variables override both, e.g. `make CONFIG_PROFILE=release`.
# ---------------------------------------------------------------------------
-include $(ROOT)/config/defaults
-include $(ROOT)/.config

CONFIG_PROFILE         ?= debug
CONFIG_EXTRA_DRIVERS   ?= n
CONFIG_MEM             ?= 256M
CONFIG_TIMEOUT         ?= 20
CONFIG_ROOTIMG         ?=
CONFIG_VERSION         ?= 0.1.0
CONFIG_INSTALL_ROOT    ?=
CONFIG_INSTALL_BOOTDIR ?= /boot

# ---------------------------------------------------------------------------
# Derived values
# ---------------------------------------------------------------------------
OUT       := target/boot
IMG       := $(OUT)/shitix.img
ELF       := $(OUT)/system.elf
SYSMAP    := $(OUT)/System.map

# Map config options -> build.sh arguments
BUILD_ARGS :=
ifeq ($(CONFIG_PROFILE),release)
  BUILD_ARGS += --release
endif
ifeq ($(CONFIG_EXTRA_DRIVERS),y)
  BUILD_ARGS += --features extra-drivers
endif

# ---------------------------------------------------------------------------
# Targets
# ---------------------------------------------------------------------------
.PHONY: all build test run debug \
        config xconfig gui menuconfig oldconfig defconfig \
        install clean mrproper showconfig help

all build:
	@bash $(ROOT)/scripts/build.sh $(BUILD_ARGS)

test:
	@MEM='$(CONFIG_MEM)' TIMEOUT='$(CONFIG_TIMEOUT)' ROOTIMG='$(CONFIG_ROOTIMG)' \
	  bash $(ROOT)/scripts/test.sh $(BUILD_ARGS)

run:
	@MEM='$(CONFIG_MEM)' ROOTIMG='$(CONFIG_ROOTIMG)' \
	  bash $(ROOT)/scripts/test.sh $(BUILD_ARGS) run

debug:
	@MEM='$(CONFIG_MEM)' ROOTIMG='$(CONFIG_ROOTIMG)' \
	  bash $(ROOT)/scripts/test.sh $(BUILD_ARGS) debug

config xconfig gui menuconfig:
	@bash $(ROOT)/scripts/menuconfig.sh

oldconfig:
	@bash $(ROOT)/scripts/oldconfig.sh

defconfig:
	@cp $(ROOT)/config/defaults $(ROOT)/.config
	@echo "  .config reset to defaults (edit with: make config | make menuconfig)"

install: all
	@CONFIG_VERSION='$(CONFIG_VERSION)' \
	  CONFIG_INSTALL_ROOT='$(CONFIG_INSTALL_ROOT)' \
	  CONFIG_INSTALL_BOOTDIR='$(CONFIG_INSTALL_BOOTDIR)' \
	  CONFIG_ROOTIMG='$(CONFIG_ROOTIMG)' \
	  bash $(ROOT)/scripts/install.sh

clean:
	@cargo clean
	@rm -rf $(OUT)
	@echo "  removed build artifacts"

mrproper: clean
	@rm -f $(ROOT)/.config
	@echo "  removed .config"

showconfig:
	@echo "  CONFIG_PROFILE         = $(CONFIG_PROFILE)"
	@echo "  CONFIG_EXTRA_DRIVERS   = $(CONFIG_EXTRA_DRIVERS)"
	@echo "  CONFIG_MEM             = $(CONFIG_MEM)"
	@echo "  CONFIG_TIMEOUT         = $(CONFIG_TIMEOUT)"
	@echo "  CONFIG_ROOTIMG         = $(CONFIG_ROOTIMG)"
	@echo "  CONFIG_VERSION         = $(CONFIG_VERSION)"
	@echo "  CONFIG_INSTALL_ROOT    = $(CONFIG_INSTALL_ROOT)"
	@echo "  CONFIG_INSTALL_BOOTDIR = $(CONFIG_INSTALL_BOOTDIR)"
	@echo "  build.sh args          = $(BUILD_ARGS)"

help:
	@printf '%s\n' \
	  'shitix build & config targets:' \
	  '' \
	  '  make            build bootable image (target/boot/shitix.img)' \
	  '  make test       build + headless QEMU boot test' \
	  '  make run        build + interactive QEMU window' \
	  '  make debug      build + QEMU waiting for GDB on :1234' \
	  '  make config     interactive terminal GUI config (xconfig/gui/menuconfig)' \
	  '  make oldconfig  merge new options from config/defaults into .config' \
	  '  make defconfig  reset .config to defaults' \
	  '  make install    build + install kernel into the LFS system (chroot)' \
	  '  make clean      remove build artifacts' \
	  '  make mrproper   clean + delete .config' \
	  '  make showconfig print the effective configuration' \
	  '' \
	  'Examples:' \
	  '  make CONFIG_PROFILE=release CONFIG_EXTRA_DRIVERS=y' \
	  '  LFS=/mnt/lfs make install        # install into an LFS root' \
	  '  ROOTIMG=target/boot/lfs.img make test'
