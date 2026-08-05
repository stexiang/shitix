# SHITIX - Rust Linux 1.0.9 Kernel

用 Rust 重写 Linux 1.0.9 内核，目标架构为 x86_64，在 QEMU 中运行。

## 项目目标

本项目旨在将 Linux 1.0.9 内核的 C 代码重写为安全的 Rust 代码，同时保持与原版功能兼容。Rust 的内存安全特性有助于防止常见的内核漏洞，同时保留对底层硬件的直接控制能力。

## 构建要求

### 必需工具

- **Rust Nightly** - 使用 `rustup` 安装：

```bash
rustup install nightly
rustup default nightly
rustup target add x86_64-unknown-none --toolchain nightly
```

- **QEMU** - 用于运行内核：

```bash
# Debian/Ubuntu
sudo apt install qemu-system-x86

# macOS (with Homebrew)
brew install qemu

# Arch Linux
sudo pacman -S qemu
```

- **NASM** - 汇编器：

```bash
# Debian/Ubuntu
sudo apt install nasm

# macOS
brew install nasm
```

### 快速构建

项目使用 `rust-toolchain.toml` 自动配置 nightly 工具链：

```bash
# 编译并运行（默认测试模式）
bash scripts/test.sh

# 交互式运行（带 QEMU 窗口）
bash scripts/test.sh run

# GDB 调试模式
bash scripts/test.sh debug
# 另开终端: gdb -ex 'target remote :1234' target/boot/system.elf

# 仅构建镜像
bash scripts/build.sh

# Release 构建
PROFILE=release scripts/test.sh
```

## 项目结构

```
shitix/
├── boot/           # 引导链汇编与链接脚本
│   ├── bootsect.S  # 512B 引导扇区
│   ├── setup.S     # 机器参数收集
│   ├── head.S      # 64 位入口
│   └── *.ld        # 链接脚本
├── src/            # Rust 内核源码
│   ├── lib.rs      # 主入口与模块定义
│   ├── desc.rs     # GDT/IDT/TSS 初始化
│   ├── sched/      # 调度器
│   ├── mm/         # 内存管理
│   ├── fs/         # 文件系统
│   ├── klib/       # 内核库（字符串、格式化）
│   ├── drivers/    # 设备驱动
│   └── net/        # 网络协议栈
├── scripts/        # 构建脚本
│   ├── build.sh    # 构建镜像
│   └── test.sh     # 自动化测试
├── linux/          # 原始 Linux 1.0.9 C 代码（参考用）
└── target/boot/   # 构建产物
    ├── shitix.img  # 磁盘镜像
    └── system.elf  # 内核 ELF
```

## 核心模块

| 模块 | 说明 | 参考源码 |
|------|------|----------|
| `desc.rs` | GDT/IDT/TSS 初始化 | `boot/head.S` |
| `sched/` | 任务调度器 | `kernel/sched.c` |
| `mm/` | 内存管理 | `mm/*.c` |
| `fs/` | 文件系统 | `fs/*.c` |
| `fs/ext2/` | ext2/ext3/ext4 文件系统 | `fs/ext2/*.c` |
| `klib/string.rs` | 字符串操作 | `lib/string.c` |
| `klib/vsprintf.rs` | 格式化输出 | `kernel/vsprintf.c` |
| `signal.rs` | 信号处理 | `kernel/signal.c` |
| `exit.rs` | 进程退出 | `kernel/exit.c` |
| `ioport.rs` | I/O 端口管理 | `kernel/ioport.c` |
| `drivers/` | 设备驱动 | `drivers/block/` |
| `net/` | 网络协议栈 | `net/` |

## 内存布局

| 地址 | 内容 |
|------|------|
| 0x90000 | 机器参数区 |
| 0x901E0 | E820 条目数 |
| 0x90200 | setup（4 扇区）|
| 0x9E000 | E820 条目数组 |
| 0x4000-0x6FFF | PML4/PDPT/PD 页表 |
| 0x10000 | system: head.S + Rust 内核 |

## 安全特性

### 安全字符串 API

`klib/string.rs` 提供两套 API：

**安全版本（推荐）：**
```rust
use crate::klib::string::*;

// 使用切片而非裸指针
let len = strlen_slice(b"hello\0");
let ord = strcmp_slice(b"abc\0", b"abd\0");
let pos = strchr_slice(b"hello\0", b'l');
let dest = memcpy_slice(&mut buf[..], b"world\0");
```

**兼容版本（保留 unsafe）：**
```rust
use crate::klib::string::*;

let len = unsafe { strlen(b"hello\0".as_ptr()) };
```

### 线程安全的 I/O 端口管理

`ioport.rs` 使用原子操作和自旋锁：

```rust
use crate::ioport::*;

fn snarf_region(base: u32, count: u32) -> Result<(), IoError>;
fn release_region(base: u32, count: u32) -> Result<(), IoError>;
fn check_port(base: u32, count: u32) -> bool;
fn get_refcount(port: u32) -> Result<u8, IoError>;
```

### ext2 文件系统

`fs/ext2/` 实现了 Linux 标准 ext2/ext3/ext4 文件系统结构：

| 结构 | 说明 |
|------|------|
| `Ext2SuperBlock` | 超级块（1024 字节，与磁盘格式完全对应） |
| `Ext2GroupDesc` | 块组描述符 |
| `Ext2Inode` | inode 结构（支持直接块和间接块） |
| `Ext2DirEntry` | 目录项结构 |

## 构建产物

- `target/boot/shitix.img` - 1MB 磁盘镜像
- `target/boot/system.elf` - 内核 ELF 文件
- `target/boot/serial.log` - 串口输出日志

## 代码规范

- 使用 `#![no_std]` 和 `#![no_main]`
- 所有 `unsafe` 块必须包含 SAFETY 注释
- 参考 `linux/` 目录下的原始 C 代码
- 优先使用安全 API（`*_slice` 函数）
- 使用 `#[repr(C)]` 确保与 C 结构体兼容

## 测试

```bash
bash scripts/test.sh
```

测试输出示例：
```
--- mm selftest ---
mm: selftest done
--- klib selftest ---
klib selftest: ALL PASS
klib: selftest done
--- net skbuff selftest ---
[PASS] net: skb_create
[PASS] net: skb_refcount
...
```

## 已知限制

- 目标架构：x86_64 only
- 运行平台：QEMU
- 不支持 SMP（单核）
- 部分 Linux 1.0.9 功能尚未实现

## 许可证

本项目代码遵循 GPL-2.0 许可证（与 Linux 1.0.9 相同）。

原始 Linux 1.0.9 代码位于 `linux/` 目录。
