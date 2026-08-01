# OpenWolf

@.wolf/OPENWOLF.md

This project uses OpenWolf for context management. Read and follow .wolf/OPENWOLF.md every session. Check .wolf/cerebrum.md before generating code. Check .wolf/anatomy.md before reading files.


# CLAUDE.md

## 项目目标
用 Rust 重写 Linux 1.0.9 内核，目标架构为 x86_64，在 QEMU 中运行。

## 构建与运行命令
- 一键构建 + 自动化启动测试：`scripts/test.sh`
- 交互式运行（带 QEMU 窗口）：`scripts/test.sh run`
- GDB 调试：`scripts/test.sh debug`，另开终端 `gdb -ex 'target remote :1234' target/boot/system.elf`
- 只构建镜像：`scripts/build.sh`（产物 `target/boot/shitix.img`）
- release 构建：`PROFILE=release scripts/test.sh`

内核编译成 staticlib，再由 `ld` 和 `boot/head.S` 链接成 system，
不使用 `bootimage`（自己实现了 bootsect/setup 引导链）。

## 代码规范
- 使用 `#![no_std]` 和 `#![no_main]`
- 所有 `unsafe` 块必须用注释说明安全性前提
- 参考 `linux/` 目录下的原始 C 代码

## 项目结构
- `boot/`: 引导链汇编与链接脚本
  - `bootsect.S` 512B 引导扇区（自搬到 0x9000，用 INT 13h/42h LBA 读盘）
  - `setup.S` 收集机器参数 + E820、开 A20、建 4 级页表、切 long mode
  - `head.S` 64 位入口，装 GDT/IDT、清 BSS、调用 `start_kernel`
  - `*.ld` 三个链接脚本
- `src/`: Rust 内核源码（`lib.rs` / `console.rs` / `serial.rs`）
- `scripts/`: `build.sh` 构建镜像，`test.sh` 一键编译测试
- `linux/`: 原始 Linux 1.0.9 C 代码（参考用）
- `target/boot/`: 构建产物（`shitix.img`、`system.elf`、`serial.log`）

## 内存布局
| 地址 | 内容 |
|------|------|
| 0x90000 | 机器参数区（光标/内存/显示模式，偏移同原版）|
| 0x901E0 | E820 条目数 |
| 0x90200 | setup（4 个扇区）|
| 0x9E000 | E820 条目数组（20 字节/条，最多 128 条）|
| 0x70000-0x72FFF | PML4 / PDPT / PD，恒等映射低 1GB（2MB 大页）|
| 0x10000 | system：head.S + Rust 内核 |
