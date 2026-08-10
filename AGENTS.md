## 项目特定知识

### 构建命令
- `bash scripts/build.sh` - 构建内核镜像
- `bash scripts/test.sh` - 构建并运行测试
- `bash scripts/test.sh run` - 交互式运行
- `bash scripts/test.sh debug` - GDB 调试

### 内存布局
| 地址 | 内容 |
|------|------|
| 0x90000 | 机器参数区 |
| 0x90200 | setup（4扇区）|
| 0x9E000 | E820 条目数组 |
| 0x4000-0x6FFF | 页表（PML4/PDPT/PD）|
| 0x10000 | system（head.S + 内核）|

### 已解决的历史问题
- ~~LLVM noalias 优化导致 super block 数据竞争~~（bug-023）
- ~~内存溢出（OOM）导致 panic~~
- ~~execve 硬编码 argv（argc=1/argv[0]="/bin/sh"）导致外部命令拿不到参数~~
- ~~sendfile 是返回 0 的存根，busybox cat 用 sendfile 时静默无输出~~
- ~~clone/fork 不复制 fd 表（clone_fds 错误地只在 CLONE_FILES 时调用），子进程无 stdin/stdout/stderr~~
- ~~COW 页面故障未处理导致 SIGSEGV~~（bug-cow）：`try_handle_cow_fault` 对
  引用计数为 0（未跟踪）的页返回 `Some(false)`，fork 后父进程写栈即被杀。
  已改为 refs<=1 直接授予写权限（保留 NX 位）。
- ~~取指故障（err bit4）在 present+user 页上未处理~~：traps.rs 现在对
  instruction-fetch 故障清除 NO_EXEC，避免代码页被误标 NX 后 SIGSEGV。
- ~~fork 后子进程取指故障 err=0x15（NX instruction-fetch）~~（bug-cow-scan）：
  `cow_copy_page_table` 旧实现按 `0x4000_0000..0xFFFF_FFFF` 线性扫描虚拟地址，
  但 BusyBox/glibc 把 ELF 装在 `0x400000`（低于 1GB），文本/数据段整段被跳过，
  fork 后子进程没有代码映射，一返回用户态即 NX 取指缺页。已改为遍历页表树
  （PML4 低半区 0..256），并对每个共享用户页 `page_ref_inc`（对应原版
  `mem_map[MAP_NR(page)]++`）。配套：`get_page_flags` 用 `e & !ADDR_MASK`
  保留 NX bit 63（旧 `e & 0xFFF` 丢 NX）；`get_free_page_locked` 把 page_ref
  初始化为 1，`free_page_locked` 归零；`try_handle_cow_fault` 边界改为规范
  用户地址 `0..0x0000_7FFF_FFFF_FFFF`，COW 复制保留原页 NX 标志。
  验证：`err=0x15` 消失；`echo`/`true`（fork+execve）正常工作。
- ~~execve 残留父进程 TLS（fs_base）~~：fork 的子进程继承 shell 的 fs_base，
  execve 重建地址空间后该值指向已失效的旧页，新程序首次 `%fs` 访问即 #GP。
  execve 现在把 `fs_base`/`gs_base` 清 0 并同步写 MSR（新程序靠 arch_prctl
  重新建 TLS）。

### 已知未解决问题
- `ls`/`cat`（exec 后做较多 malloc 的命令）在 `rip=0x42c08e` 触发
  `general protection: sig 11 err=0x0`（#GP，非 #PF）。故障指令
  `mov -0x8(%rdi),%rax`，rdi 为非规范垃圾指针（如 `0x7410473b4838772b`），
  是 glibc malloc fastbin/tcache 取到腐败 chunk。`true`（几乎不 malloc）
  正常，故 fork+execve+COW 链路本身没问题；这是 brk/malloc 堆布局或
  glibc 交互的独立 bug，COW 修复后才暴露出来（之前子进程死在 err=0x15）。

### 调试输出规范
- 写路径/系统调用路径的调试串口打印必须用 `pr_debug!`/`pr_warn!` 等
  分级宏，不要直接 `serial::print` 留 `0xNN(a0,a1)=ret` 之类的 trace。
- execve 里不要留 `EXEC: phdr/interp/LOAD/success` 之类的 trace 打印。

### 构建与运行（含 LFS busybox 测试）
- 默认 `scripts/build.sh` 不含 IDE 驱动（`extra-drivers` feature），
  LFS 模式会报「IDE not found」。
- 要跑 busybox shell：`bash scripts/build.sh --release --features extra-drivers`
  （debug+extra-drivers 会让 system 超过 0x9F000 安全区，链接 ASSERT 失败，
  必须用 release）。
- 用第二块 IDE 盘挂 lfs3.img 跑 shell：
  ```
  qemu-system-x86_64 \
    -drive format=raw,file=target/boot/shitix.img,if=ide \
    -drive format=raw,file=lfs3.img,if=ide \
    -m 256M -no-reboot -no-shutdown -display none -serial mon:stdio
  ```
- 环境需 nightly Rust（`rustup` 安装）+ `x86_64-unknown-none` target，
  以及 `as`/`ld`/`objcopy`（binutils）和 `qemu-system-x86_64`。

### 分支状态
- `fs`: 修复了 super_block.rs 的 guard 位置 bug
- `feat/networking-stack-rewrite`: 创建了网络栈骨架（已 push）
- `feat/drivers-misc-rewrite`: 重写剩余驱动和杂项代码（新分支）

### 驱动和杂项重写计划

#### 待重写的块设备驱动 (linux/drivers/block/)
| 文件 | 说明 | 优先级 |
|------|------|--------|
| `hd.c` | IDE 硬盘控制器 | 高 |
| `floppy.c` | 软盘控制器 | 低 |
| `genhd.c` | 通用磁盘接口 | 高 |

#### 待重写的字符设备 (linux/drivers/char/)
| 文件 | 说明 | 优先级 |
|------|------|--------|
| `serial.c` | 串口 (8250 UART) | 高 |
| `lp.c` | 并口打印机 | 低 |
| `psaux.c` | PS/2 鼠标 | 中 |
| `pty.c` | 伪终端 | 中 |

#### 待重写的网络设备驱动 (linux/drivers/net/)
| 文件 | 说明 | 优先级 |
|------|------|--------|
| `ne.c` | NE2000 网卡 | 高 |
| `3c509.c` | 3COM 以太网卡 | 中 |
| `slip.c` | SLIP 协议 | 中 |
| `plip.c` | 并口 IP | 低 |
| `skeleton.c` | 驱动骨架参考 | 参考 |

#### 内核杂项 (linux/kernel/)
| 文件 | 说明 | 优先级 |
|------|------|--------|
| `signal.c` | 信号处理 | 高 |
| `exit.c` | 进程退出 | 高 |
| `info.c` | 系统信息 | 低 |
| `ioport.c` | I/O 端口访问 | 中 |
| `module.c` | 模块加载 | 低 |

### 网络栈实现状态
已添加骨架代码（2026-08-05）：
- `src/net/mod.rs` - 模块根
- `src/net/inet/skbuff.rs` - Socket buffer
- `src/net/inet/sock.rs` - Socket/协议控制块
- `src/net/inet/ip.rs` - IP 协议
- `src/net/inet/tcp.rs` - TCP 协议存根
- `src/net/inet/udp.rs` - UDP 协议存根
- `src/net/inet/icmp.rs` - ICMP 协议存根
- `src/net/inet/arp.rs` - ARP 缓存
- `src/net/inet/route.rs` - 路由表
- `src/net/inet/protocol.rs` - 协议注册
- `src/net/inet/eth.rs` - Ethernet 处理
- `src/net/inet/dev.rs` - 网络设备接口
- `src/net/unix/mod.rs` - Unix domain socket
- `src/net/tests.rs` - 自检测试（17 个测试全部通过）

自检测试覆盖：
- SkBuff: 创建、引用计数、队列、头部大小
- IP: 校验和、地址转换、协议常量
- Ethernet: 协议常量、广播/多播检测
- ARP: 协议常量、缓存操作
- 路由: 表创建、查找
- Socket: 状态枚举、标志、哈希表

TODO:
- [ ] 实现 slab 分配器
- [ ] 完成协议处理器
- [ ] 添加设备驱动
- [ ] 集成 syscall 层

### COW / WP / fork 正确性修复（2026-08-09）

#### 已落地的修复
- **CR0.WP=1**（boot/setup.S）：原 setup.S 进 long mode 时只置 PG|PE，没置 WP（bit 16）。WP=0 时内核态写忽略页表 R/W 位，fork 后所有用户页被 cow_copy_page_table 标成只读，此时内核的 copy_to_user（read 等）与 clone 的 SETTID/CLEARTID 会直接写穿共享物理页、绕过写时复制。Linux 在 start_kernel 早期就置 WP=1，这里对齐。
- **内核态 COW 缺页处理**（src/traps.rs）：WP=1 后内核对 COW 只读用户页的写会以 supervisor write-protect 缺页（err P=1 W=1 U=0）。do_trap 在 die_if_kernel 之前加了这条路径：调用 try_handle_cow_fault 复制出私有页后重试写。
- **CLONE_CHILD_SETTID 延迟到子进程**（src/sched/task.rs 加 set_child_tid 字段，src/sched/mod.rs:schedule_tail 写入）：原 clone 在父进程上下文里 write_volatile(child_tidptr, pid)，COW 下会改穿父进程的页。改为存地址、由子进程在 ret_from_fork 路径自己 put_user，对齐 Linux。
- **CLONE_SETTLS 不再 wrmsr 父进程**（src/syscall/sys.rs）：原 clone 在父进程上下文 wrmsr 写子进程的 FS_BASE，立即破坏父进程 TLS。改为存 child.fs_base，由 switch_to 切到子进程时写 MSR。
- **COW 只处理真正的用户页**（src/umm/mod.rs:try_handle_cow_fault）：加 is_user_mapped 门槛。translate 只看 PRESENT，会把内核低地址恒等映射拆出的 present-but-not-user 表项也当已映射，导致用户态对近 NULL 地址的写被当成 COW 改成可写/放行，把本该 SIGSEGV 的空指针解引用静默吞掉。

#### ls/cat 仍未通的根因（待办）
ls/cat 在 fork 子进程的 glibc __libc_malloc 里 #GP（rip=0x42c08e）。根因链：
1. execve 映了第 0 页 USER|RW（src/syscall/sys.rs 第 7 步），为让 busybox 静态 glibc 早期 init 的空指针访问不 SIGSEGV 的临时缓解。
2. fork 子进程 glibc robust-list 初始化读 %%fs:0x10（pthread 自指针）为 0，随后 movups %%xmm0,0x2d8(%%rax) 写到 NULL+0x2d8，写穿第 0 页，级联腐败 malloc arena。
3. 不映第 0 页时该写正常 SIGSEGV，但又会暴露父进程在子进程退出后 RIP 被打成 0（疑似 SIGCHLD 投递，signal.c 未移植）。

根治需要：(a) 排查 glibc TLS 自指针 %%fs:0x10 为何为 0（execve auxv/TLS 布局）；(b) 移植 signal.c。两项做完即可删掉第 0 页映射、恢复 NULL 保护。

#### 验证状态
- selftest 全过，boot ok，echo/true 正常。
- ls/cat 仍 #GP（见上）。
- 调试打印（W: rw_ret / CHR: major / TTY_W:）已全部移除（commits a123e6d / a66def9），boot 输出 grep 计数为 0。

### 符号链接（symlink）支持（2026-08-10）

LFS merged-usr 布局用 `/sbin`->`usr/sbin`、`/bin`->`usr/bin` 等符号链接，
内核缺 `follow_link` 导致 execve 对 `/sbin/init`、`/bin/sh` 返回 ENOTDIR(-20)。

#### 已落地的修复
- per-task `link_count: u8`（src/sched/task.rs，empty() 置 0）：防环守卫，
  超过 5 层返回 -ELOOP（同原版 current->link_count）。
- ext4 symlink target reader（src/fs/ext4/ops.rs:read_symlink）：
  读 raw inode，用 Ext4Inode::is_fast_symlink()（i_blocks==0 && size<=60）
  区分快速链接（目标内联在 i_block 前 i_size 字节）与慢链接（bmap_phys
  求第一块物理号后 bread 读数据块）。注意：判快速链接必须用 is_fast_symlink()
  而非 bmap_phys!=0——快速链接的 i_block 区存的是目标文本（如 "usr/sbin"），
  不是 extent 树，bmap_phys 会按经典 direct-block 解释 ib[0..4] 得到天文数字块号，
  bread 挂死。
- follow_link + _namei/dir_namei_base 跟随符号链接（src/fs/namei.rs）：
  port 自 linux-1.0.9 namei.c。中间分量与末尾分量都跟随；lnamei（follow_links=false）
  给 lstat/readlink 用，不跟随末尾。相对链接以链接所在目录为解析起点。
- sys_lstat 改用 lnamei（src/fs/stat.rs）：原 lstat 直接转调 stat，现在正确返回链接自身 inode。
- readlink 改用 lnamei（src/syscall/sys.rs）：取链接自身而非目标。

#### 验证
- merged.img（merged-usr，含 /sbin->usr/sbin、/bin->usr/bin、/lib64->usr/lib，
  及 loopa<->loopb 环链）：/sbin/init 经 symlink 解析后 execve 成功
  （busybox init 输出 "init: must be run as PID 1"）；/loopa 返回 -40 (ELOOP)。
- lfs3.img（真实目录布局）回归：/sbin/init 正常 execve，无回归。
- 默认 ramdisk 引导：全部 selftest 通过，boot ok。

#### 限制
- minix 符号链接未移植：read_symlink_target 对 minix 链接返回 None -> -EIO。
- O_NOFOLLOW 未移植：open_namei 对已存在的末尾链接统一跟随（同原版 1.0.9）。
