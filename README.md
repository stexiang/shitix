# SHITIX — Rust 重写的 Linux 1.0.9 内核

用 Rust 重写 Linux 1.0.9 内核，目标架构 x86_64，在 QEMU 中运行。
现已支持 ring-3 用户态切换、ext4 文件系统完整读写、SoundBlaster/AdLib 声卡驱动。

## 快速开始

```bash
# 一键编译 + 测试（debug）
bash scripts/test.sh

# Release + 完整驱动（声卡、IDE、ext4 完整 ops）
bash scripts/test.sh --release --features extra-drivers

# 交互式运行（带 QEMU 窗口）
bash scripts/test.sh --release --features extra-drivers run

# GDB 调试
bash scripts/test.sh debug
# 另开终端: gdb -ex 'target remote :1234' target/boot/system.elf

# 仅构建镜像
bash scripts/build.sh
bash scripts/build.sh --release --features extra-drivers
```

## 构建要求

- **Rust Nightly**（`rust-toolchain.toml` 自动配置）
- **GNU as + ld**（binutils）
- **QEMU** (`qemu-system-x86_64`)
- **Python 3**（镜像组装用）

```bash
# Debian/Ubuntu
sudo apt install qemu-system-x86 binutils python3
rustup install nightly
rustup target add x86_64-unknown-none --toolchain nightly
```

## 项目结构

```
shitix/
├── boot/              # 引导链汇编 + 链接脚本
│   ├── bootsect.S     # 512B 引导扇区（LBA 读盘）
│   ├── setup.S        # E820 探测、A20、4 级页表、切 long mode
│   ├── head.S         # 64 位入口（GDT/IDT/BSS/call start_kernel）
│   ├── entry.S        # 中断/异常/system_call 入口（SAVE_ALL/RESTORE_ALL）
│   └── *.ld           # 三个链接脚本
├── src/               # Rust 内核源码
│   ├── lib.rs         # 入口 + 启动自检（mm/klib/trap/syscall/sched/fs/ext4/fork/ring3）
│   ├── console.rs     # VGA 文本控制台（0xB8000, 80×25, 16 色, fmt::Write）
│   ├── serial.rs      # COM1 串口输出
│   ├── desc.rs        # GDT（7 项）/ TSS（rsp0+3 IST）/ IDT（256 门）
│   ├── traps.rs       # 异常处理（21 异常 + PtRegs）
│   ├── irq.rs         # 8259A PIC + bottom half
│   ├── syscall/       # 系统调用（x86_64 正式表 0..=334, 361 wired）
│   ├── sched/         # 进程调度（task/mod）+ 内核栈池
│   ├── signal.rs      # 信号处理
│   ├── exit.rs        # 进程退出 + wait4
│   ├── mm/            # 内存管理
│   │   ├── page.rs    # PAGE_SIZE / 对齐
│   │   ├── page_alloc.rs  # 页帧分配器（mem_map, get_free_page）
│   │   ├── kmalloc.rs     # 小块分配器（8 档）
│   │   ├── paging.rs      # 四级页表（map/unmap/translate）
│   │   ├── page_ref.rs    # 引用计数 + COW
│   │   └── area.rs        # verify_area / copy_from_user
│   ├── fs/            # 虚拟文件系统
│   │   ├── buffer.rs      # 缓冲区高速缓存（64 缓冲）
│   │   ├── inode.rs       # inode 表（64 槽）
│   │   ├── super_block.rs # 超级块管理
│   │   ├── namei.rs       # 路径解析（open/create/mkdir/rmdir…）
│   │   ├── open.rs        # fd 管理
│   │   ├── read_write.rs  # 读写
│   │   ├── devices.rs     # 设备文件分派
│   │   ├── stat.rs        # stat/fstat
│   │   ├── ext2/          # ext2 结构定义
│   │   ├── ext4/          # ext4 完整实现
│   │   │   ├── super_block.rs  # ext4 超级块
│   │   │   ├── inode.rs        # ext4 inode（256B, 64位时间戳）
│   │   │   ├── extent.rs       # extent 树（B 树索引）
│   │   │   ├── group_desc.rs   # 块组描述符
│   │   │   ├── dir.rs          # ext4_dir_entry_2 + DirIter
│   │   │   ├── bitmap.rs       # 块/inode 位图分配
│   │   │   ├── ops.rs          # VFS 操作（read/write/bmap/balloc/truncate）
│   │   │   ├── namei.rs        # 目录操作（lookup/create/mkdir/rmdir/unlink）
│   │   │   ├── mkfs.rs         # 内存中创建 ext4 文件系统
│   │   │   └── selftest.rs     # 51 项结构自检
│   │   └── minix/          # minix v1 文件系统（完整 + mkfs）
│   ├── drivers/        # 设备驱动
│   │   ├── block/          # 块设备
│   │   │   ├── ll_rw.rs        # 请求队列
│   │   │   ├── ramdisk.rs      # 内存盘（ROOT_DEV）
│   │   │   ├── genhd.rs        # 通用磁盘
│   │   │   └── hd.rs           # IDE 硬盘（ATA PIO）
│   │   ├── char_dev/       # 字符设备
│   │   │   ├── tty.rs          # tty + 行规则
│   │   │   ├── console.rs      # tty→VGA 输出端
│   │   │   ├── keyboard.rs     # PS/2 键盘（IRQ1）
│   │   │   └── mem.rs          # /dev/null /dev/zero /dev/mem
│   │   └── sound/          # 声卡子系统
│   │       ├── config.rs       # 常量/类型/寄存器偏移
│   │       ├── dev_table.rs    # 设备虚表（audio/mixer/synth/midi ops）
│   │       ├── soundcard.rs    # 声卡入口（注册/探测/中断）
│   │       ├── sound_switch.rs # VFS 分派（按次设备号）
│   │       ├── dmabuf.rs       # DMA 缓冲区管理（物理缓冲/环形队列）
│   │       ├── audio.rs        # /dev/dsp + /dev/audio + μ-law 编解码
│   │       ├── opl3.rs         # YM3812/YMF262 FM 合成器
│   │       ├── sb.rs           # SoundBlaster DSP + mixer
│   │       └── adlib.rs        # AdLib 卡片（OPL-2 检测）
│   ├── umm/            # 用户态内存管理（vm_area + COW）
│   ├── net/            # 网络协议栈
│   │   ├── inet/           # ARP/IP/ICMP/UDP/TCP/Ethernet/route
│   │   └── unix/           # Unix 域套接字
│   ├── klib/           # 内核基础库
│   │   ├── ctype.rs        # 字符分类
│   │   ├── string.rs       # C 字符串/内存操作
│   │   ├── errno.rs        # 错误码（122 个）
│   │   ├── vsprintf.rs     # 格式化输出
│   │   └── printk.rs       # 环形日志 + 级别过滤
│   ├── elf/            # ELF32/64 解析
│   ├── pci/            # PCI 枚举
│   ├── usb/            # USB 驱动栈
│   ├── smp/            # SMP/APIC 支持
│   └── ioport.rs       # I/O 端口管理
├── scripts/           # 构建工具
│   ├── build.sh        # 构建镜像（--release, --features）
│   └── test.sh         # 构建 + QEMU 无头启动 + 串口校验
├── linux/             # 原始 Linux 1.0.9 C 代码（参考）
└── target/boot/       # 产物
    ├── shitix.img      # 磁盘镜像（1MB 对齐）
    └── system.elf      # 内核 ELF
```

## 自检覆盖

| 自检 | 内容 | 状态 |
|------|------|------|
| mm | 页分配去重清零、kmalloc 6 档读写、页表 map/translate/unmap | PASS |
| klib | ctype 边界、string 全函数、number 补位、strtoul、vsprintf 截断、printk 过滤 | PASS |
| traps | int3、除零、无效 opcode 现场打印 + 恢复 | PASS |
| syscall | getpid/getppid/越界/未实现/write/uname、361 wired | PASS |
| sched | timer 20 ticks、2 内核线程各 14 轮、42 次上下文切换 | PASS |
| net | 17 项：skb/ip/eth/arp/route/sock 常量和操作 | PASS |
| fs | 块 I/O、超级块、readdir、9KB 一级间接、mkdir/rmdir/unlink、/dev/zero、缓冲去重 | PASS |
| ext4 structure | 51 项：超级块/GroupDesc/inode/extent/DirIter | PASS |
| ext4 creation | open/write/lseek/read/fstat/dup/close/stat/mkdir/rmdir/unlink/EFAULT guards | PASS |
| fork/exit/wait4 | fork + SIGTERM default action | PASS |
| ring-3 | getpid→exit(42) roundtrip、slot+PML4 freed | PASS |

## 编译特性

```bash
# 默认：基础内核（最小 ext4 ops，不含声卡/IDE/完整 ext4）
scripts/test.sh

# extra-drivers：完整 ext4 + 声卡 + IDE 硬盘
scripts/test.sh --release --features extra-drivers
```

`extra-drivers` feature 额外启用：
- **ext4 完整实现**：`read_inode`/`write_inode`/`bmap`/`balloc`/`truncate`/`file_read`/`file_write`
- **ext4 目录操作**：`lookup`/`create`/`mkdir`/`rmdir`/`unlink`/`link`
- **声卡子系统**：SoundBlaster DSP+Mixer、AdLib OPL-2、/dev/dsp、/dev/audio、DMA 缓冲管理
- **IDE 硬盘驱动**：ATA PIO 扇区读写

> 默认 debug 构建受 `0x91000` 镜像上限约束，不含以上模块。Release 构建 LTO 可容纳全功能。

## 内存布局

| 地址 | 内容 |
|------|------|
| 0x90000 | 机器参数区（光标/内存/显示模式） |
| 0x901E0 | E820 条目数 |
| 0x90200 | setup（4 扇区） |
| 0x9E000 | E820 条目数组（20 字节/条，最多 128 条） |
| 0x4000-0x6FFF | PML4/PDPT/PD 页表（恒等映射低 1GB / 2MB 大页） |
| 0x10000 | system：head.S + entry.S + Rust 内核 |

## 代码规范

- `#![no_std]` + `#![no_main]`
- 所有 `unsafe` 块上方写 `// SAFETY:` 注释说明前提
- `unsafe fn` 写 `# Safety` 文档段
- MMIO 一律 `read_volatile` / `write_volatile`
- 引用 Linux 1.0.9 原始 C 代码位置
- 每个模块末尾自检（启动期跑，事后可删）

## LFS 集成

shitix 可以在 QEMU 中启动 LFS (Linux From Scratch) 根文件系统。
以下假设你已按 LFS 手册构建了 `$LFS` 目录树。

### 制作 ext4 磁盘镜像

```bash
# 创建 128MB 镜像
dd if=/dev/zero of=lfs.img bs=1M count=128

# 格式化为 ext4（关闭 64bit/huge_file 等需要较新内核的特性）
mkfs.ext4 -F -O ^64bit,^huge_file,^flex_bg,^metadata_csum lfs.img
# 或更激进：只保留 extent + filetype
mkfs.ext4 -F -O ^64bit,^huge_file,^flex_bg,^metadata_csum,^dir_index,^has_journal lfs.img

# 挂载并拷贝 LFS 系统
sudo mount lfs.img /mnt
sudo cp -a $LFS/* /mnt/
sudo umount /mnt
```

### 构建内核并启动

```bash
# 构建带完整 ext4 支持的内核
bash scripts/build.sh --release --features extra-drivers

# 将 ext4 镜像追加到内核镜像后面
# shitix.img 是 1MB 对齐的，lfs.img 从下一个 1MB 边界开始
cat target/boot/shitix.img lfs.img > bootable.img

# QEMU 启动（使用 IDE 硬盘）
qemu-system-x86_64 \
    -drive format=raw,file=bootable.img,if=ide \
    -m 512M \
    -no-reboot \
    -serial stdio
```

内核探测到 IDE 硬盘上第二个分区（lfs.img）并尝试将其挂载为根文件系统。
也可以使用 `-hda` 加载内核镜像，`-hdb` 加载 ext4 镜像：

```bash
qemu-system-x86_64 \
    -drive format=raw,file=target/boot/shitix.img,if=ide,index=0 \
    -drive format=raw,file=lfs.img,if=ide,index=1 \
    -m 512M -no-reboot -serial stdio
```

### 当前能力

| 功能 | 状态 |
|------|------|
| ext4 读（目录遍历、文件读取） | ✓ |
| ext4 写（创建/删除文件与目录） | ✓ |
| ring-3 用户态切换（iretq） | ✓ |
| int 0x80 系统调用 | ✓ |
| 信号投递（SIGSEGV/SIGTERM 等） | ✓ |
| fork + COW 页表复制 | ✓ |
| ELF64 加载器 | ✓ |
| ELF64 execve | 进行中（Stage 4） |
| `syscall` 指令入口 | 桩（cli; hlt） |
| `arch_prctl`（FS/GS base, TLS） | 未实现 |
| x86_64 `struct stat` ABI | 当前为 i386 布局 |
| 动态链接器 (ld.so) | 结构支持，ABI 待补 |
| 网络协议栈 | ARP/IP/ICMP/UDP/TCP 结构定义，未接驱动 |

### 已知限制

要在 LFS 用户态下运行 `/bin/bash` 等程序，以下功能需要先补齐：

1. **`sys_execve`** — 当前返回 `-ENOSYS`，init 进程无法执行用户态二进制
2. **`arch_prctl(ARCH_SET_FS)`** — glibc 启动第一条系统调用，用于装 TLS 指针
3. **x86_64 `struct stat`** — `fstat` 等返回的结构体是 i386 布局，glibc 会误解
4. **动态链接器支持** — 需要 `sys_mmap` + ELF 解释器加载
5. **`syscall` 指令入口** — LFS 的 x86_64 glibc 使用 `syscall` 指令而非 `int 0x80`

### 路径规划

1. **Stage 4** — ELF64 + execve：让内核能 `iretq` 到用户态运行 `/init`
2. **Stage 5** — ABI 补齐：`arch_prctl`、`stat`、`mmap`、动态链接
3. **Stage 6** — 设备驱动完善：串口 tty、IDE DMA
4. **Stage 7** — `syscall` 指令启用 + `getdents64` 等系统调用补齐

## 许可证

GPL-2.0（与 Linux 1.0.9 相同）。原始代码在 `linux/` 目录。
