# SHITIX — Rust 重写的 Linux 1.0.9 内核

用 Rust 重写 Linux 1.0.9 内核，目标架构 x86_64，在 QEMU 中运行。
现已支持：ELF64 execve、ring-3 用户态、ext4 完整读写、SoundBlaster/AdLib 声卡。

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
| smp | 13 register constants, 3 DeliveryMode, CpuInfo states, CPU count, LAPIC base | PASS |
| fork/exit/wait4 | fork + SIGTERM default action | PASS |
| ring-3 | getpid→exit(42) roundtrip、slot+PML4 freed | PASS |
| execve | ELF64 from ext4 → user-mode → getpid+exit(42) | PASS |

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
| ELF64 execve | ✓ |
| Static ELF loading (no ld.so) | ✓ |
| `syscall` 指令入口 | ✓（MSR STAR/LSTAR/SFMASK 已配置，entry.S 就绪） |
| `arch_prctl`（FS/GS base, TLS） | ✓（ARCH_SET_FS/GET_FS/GS 通过 wrmsr） |
| x86_64 `struct stat` ABI | ✓（Stat64，144 字节，glibc 兼容） |
| 动态链接器 (ld.so) | 结构支持（PT_INTERP 加载），mmap 待补 |
| 网络协议栈 | ARP/IP/ICMP/UDP/TCP 结构定义，未接驱动 |

### 已知限制

#### 阻塞 LFS 用户态的关键缺口

要在 LFS 用户态下运行 `/bin/bash` 等程序，以下功能需要先补齐：

| 缺口 | 影响 | 当前状态 |
|------|------|----------|
| 动态链接器 (ld.so) | 需 `sys_mmap` 加载 ELF 段 | `mmap` 为 `-ENOSYS`，PT_INTERP 加载已就绪 |

#### 架构与内核基础设施

| 缺口 | 影响 | 当前状态 |
|------|------|----------|
| per-task file descriptor 表 | `do_exit` 里的 `close_all()` 操作全局 `FD_TABLE`，fork 后父子共用 fd | 全局静态数组 `FD_TABLE: [Option<usize>; 32]` |
| per-task `pwd`/`root` | `chdir` 影响所有任务，多进程环境下路径解析互踩 | `super_block::pwd_inode()` / `root_inode()` 为全局状态 |
| `clone` flags 语义 | `sys_clone` 无法创建线程（无 `CLONE_VM`/`CLONE_FILES` 等） | 返回 `-ENOSYS` |
| POSIX 线程 (futex) | glibc `pthread_create` 依赖 `sys_futex` | `-ENOSYS`（缺 per-address 等待队列） |
| POSIX 定时器 | `timer_create`/`timer_settime` 等未实现 | 全部 `-ENOSYS` |
| 实时信号 (rt_sig*) | 缺用户态信号栈帧 (`setup_frame`/`sigreturn`)，无法投递带 `siginfo_t` 的信号 | 基本信号投递工作（`SIG_DFL`/`SIG_IGN`），`rt_sigaction` 等返回 `-ENOSYS` |
| ANSI 终端转义序列 | 控制台只处理 `\n\r\t\b`，无 `csi_J`/`csi_K`/颜色序列等 VT102 控制 | `drivers/char_dev/console.rs` 为最小实现，`/dev/tty` 不支持 `ioctl` |

#### 系统调用覆盖

现有 341 个已接线调用中，89 个为 `-ENOSYS` 占位（大部分注明了所缺子系统）。
其余返回合理默认值（如 `madvise`/`readahead` 忽略、`getgroups` 返回 0）。

| 类别 | 缺失调用 | 数量 |
|------|---------|------|
| 信号 | `rt_sigaction`, `rt_sigprocmask`, `rt_sigreturn`, `sigaltstack` 等 | 10 |
| 定时器 | `timer_create`, `timer_settime`, `clock_gettime` 等 | 8 |
| 同步 | `futex`, `set_robust_list`, `get_robust_list` | 3 |
| 进程 | `clone`, `clone3`, `execve`, `execveat` | 4 |
| 内存 | `mmap`, `munmap`, `mprotect`, `mremap`, `msync` 等 | 8 |
| 文件 | `sendfile`, `splice`, `copy_file_range`, `sync_file_range` | 4 |
| 网络 | `socket`, `bind`, `connect`, `listen`, `accept` 等 | 18 |
| I/O | `io_uring_setup`, `io_uring_enter`, `io_uring_register` | 3 |
| IPC | `msgget`, `semget`, `shmget` 等 | 10 |
| 其他 | `ptrace`, `iopl`, `ioperm`, `kexec_load` 等 | 21 |

#### 驱动与硬件

| 缺口 | 当前状态 |
|------|----------|
| 串口 tty (`/dev/ttyS0`) | `src/serial.rs` 为单向输出端，无中断接收、无 tty 语义 |
| IDE DMA | `src/drivers/block/hd.rs` 仅 PIO 模式 |
| 软盘 | 未移植（`linux/drivers/block/floppy.c` ~1800 行状态机） |
| SCSI | 未移植（完整子系统 ~20 文件） |
| 网络设备驱动 | 协议栈结构就绪，无网卡驱动（NE2000/3c509/e1000 等） |
| 声卡录音 (DMA input) | `dmabuf.rs` 输出路径就绪，输入路径为桩 |
| SMP 多核 | LAPIC MMIO 基址可设，AP 启动流程已编码，缺 page table 映射和真实启动验证 |

### 路径规划

#### Stage 4 — ELF64 + execve ✅ （已完成）

- `sys_execve`：`namei` → ELF 解析 → PT_LOAD 段映射 → auxv 栈初始化 → CR3 切换 → iretq
- PT_INTERP 动态链接器加载（打开解释器文件 → 映射 PT_LOAD 段 → 设置 AT_BASE）
- auxv 向量（AT_PHDR/AT_PHENT/AT_PHNUM/AT_PAGESZ/AT_ENTRY/AT_BASE/AT_NULL）
- 最小 ELF64 构造器（`build_minimal_elf64`），用于内核实测
- 已验证：从 ext4 加载 ELF → iretq 到 ring-3 → 执行 `getpid()+exit(42)` → 退出码 42

#### Stage 5 — ABI 补齐

- **`arch_prctl(ARCH_SET_FS/ARCH_GET_FS)`**：per-task FS.base，glibc TLS 的关键依赖
- **x86_64 `struct stat`**：扩展为 64 位字段（`st_dev`/`st_ino`/`st_size`/`st_blocks` 等），兼容 glibc
- **动态链接**：`sys_mmap` + ELF PT_INTERP 路径解析 → ld.so 映射
- **`syscall` 指令**：配置 MSR STAR（0xC0000081）/LSTAR（0xC0000082）/SFMASK（0xC0000084），swapgs，per-task 用户栈暂存
- 补齐 `rt_sigaction`/`sigaltstack`/`sigreturn` 信号栈帧

#### Stage 6 — 进程模型完善

- **per-task fd 表**：将 `FD_TABLE` 从全局静态数组改为 `Task` 内字段，`fork` 时拷贝
- **per-task `pwd`/`root`**：`fs_struct` 迁移到 `Task`
- **`clone` flags**：`CLONE_VM`/`CLONE_FILES`/`CLONE_SIGHAND` 等，支持线程创建
- **`sys_futex`**：per-address 等待队列，glibc pthread 互斥锁的基础
- **`sys_wait4` 完善**：`WNOHANG`/`WUNTRACED` + 退出码编码修复（当前 exit(1..31) 被误判为信号）

#### Stage 7 — 控制台与交互

- **VT102 转义序列**：`csi_J`（清屏）、`csi_K`（清行）、`csi_m`（SGR 颜色）、光标定位
- **串口 tty**：UART 中断接收 + tty 队列，支持 `/dev/ttyS0` 作为交互终端
- **`/dev/tty` ioctl**：`TCGETS`/`TCSETS`/`TIOCGWINSZ` 等，bash 需要
- **伪终端 (pty)**：成对 tty + `sys_openpty`，SSH/tmux 的基础

#### Stage 8 — 网络

- **网卡驱动**：e1000（QEMU 默认）或 NE2000
- **socket 系统调用**：`socket`/`bind`/`connect`/`listen`/`accept`/`send`/`recv`
- **TCP 状态机**：协议栈结构已有，需接 syscall 层和驱动收发包
- **loopback 设备**：127.0.0.1 本地通信

#### 长期

- SMP 多核启动（APIC timer 替代 PIT、IPI、per-CPU 结构）
- SATA/AHCI 驱动
- NVMe 驱动
- USB HID（键盘/鼠标）
- VESA framebuffer + 图形控制台
- 信号驱动的异步 I/O
- cgroup / namespace 基础

## 许可证

GPL-2.0（与 Linux 1.0.9 相同）。原始代码在 `linux/` 目录。
