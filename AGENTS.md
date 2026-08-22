## 项目特定知识

### 构建命令
- `bash scripts/build.sh` - 构建内核镜像
- `bash scripts/test.sh` - 构建并运行测试
- `bash scripts/test.sh run` - 交互式运行
- `bash scripts/test.sh debug` - GDB 调试

### 内存布局
| 地址 | 内容 |
|------|------|
| 0x2000 | SMP AP 蹦床页（代码在头部，0x2F00 起为启动信箱）|
| 0x90000 | 机器参数区 |
| 0x90200 | setup（4扇区）|
| 0x9E000 | E820 条目数组 |
| 0x4000-0x6FFF | 页表（PML4/PDPT/PD）|
| 0x10000 | system（head.S + 内核）|
| 0xffff800040000000+ | AP 启动栈（每核 16KB 有效 + 16KB guard 洞）|

### 已解决的历史问题
- ~~LLVM noalias 优化导致 super block 数据竞争~~（bug-023）

### 本次会话修复（让内核跑通完整 glibc LFS，2026-08-17）

基线：musl busybox(lfs3.img) 能 boot，但 glibc 动态链接完全跑不起来。
逐个定位并修复，最终 glibc bash + fork/exec `ls` + 信号投递全链路通过：

1. **init 不是 PID 1**：`sched::kernel_thread` 直接读 `LAST_PID`（不增），
   而 sched_selftest 的 worker 线程先抢走 pid 1/2，init(fs_init_thread)
   拿到 pid 3，busybox/sysvinit 报 `must be run as PID 1`。加
   `sched::reset_last_pid()`，在创建 init 线程前清零计数器。
2. **动态链接器（ld.so）文本段没加载**：execve 的 PT_INTERP 加载只用
   `read()` 读了一页进 `ibuf`，再从这一页按 `ifoff+..` 拷贝——只要段在文件
   偏移 >= 一页（glibc ld-linux 的 `.text` 在 offset 0x1000），条件恒假，
   整段文本装成零页。改成 `lseek(ifd)+循环 read`（对齐主加载器）。
3. **初始栈没 16 字节对齐**：SysV ABI 要求入口 %rsp%16==0；不齐会让 ld.so
   `_dl_start` 的 `movaps` 触发 #GP。aux_top 改 16 对齐 + 顶部按需垫一槽。
4. **AT_ENTRY 传错**：execve 把 `entry` 先存主程序入口、加载解释器后又覆盖成
   ld.so 入口，导致 auxv 的 AT_ENTRY=ld.so 自己入口。加 `main_entry` 变量。
5. **check_range 3GB 上限拒绝高位地址**：动态链接器/共享库被映到 0x7f_…
   高位，`user_buf` 的 3GB 护栏把 ld.so `.rodata` 里的 writev iovec 全挡成
   -EFAULT（glibc 错误只打出程序名）。改成「低 3GB 恒等映射直过 + 高位走
   `verify_area` 查页表」。
6. **pread64/pwrite64 是返回 0 的存根**：glibc `_dl_map_object_from_fd` 用
   `__pread64_nocancel` 读共享库 ELF 头/程序头，存根返回 0 →「cannot read
   file data」。实现成 lseek+read+恢复 f_pos。
7. **mmap 文件映射读错偏移**：file-backed mmap 靠 `read(fd)` 的 f_pos 推进，
   只对 offset==0 连续页碰巧对；动态链接器按段偏移 mmap libc 读到错页。
   改成 lseek(file_off)+read，并允许越过文件末尾（BSS 读作 0）。
8. **phdr_prot_to_flags 从不设 NX**：PF_X/PF_R 都被当 PRESENT 用，数据段
   全部可执行。改成 `PRESENT|USER + (PF_W→RW) + (!PF_X→NO_EXEC)`。
9. **mmap/brk 是 eager 分配**：glibc malloc 初始化时 `mmap(NULL, ~1.4GB,
   RW, ANON)` 预留主竞技场，Linux 下只占虚拟空间；内核每页都 get_free_page，
   256MB 内存一次吃光、fork 全部 EAGAIN。实现惰性分配：`map_reserved`（叶子
   PTE PRESENT=0+RESERVED bit9）+ 缺页处理里 `resolve_reserved` 按需落实，
   fork 的 cow_copy_page_table 也复制 RESERVED 页。
10. **rt_sigprocmask 三个操作符全写反**：SIG_BLOCK 写成覆盖、SIG_UNBLOCK
    写成或、SIG_SETMASK 写成 old&~set，导致 glibc 启动后几乎所有信号（含
    SIGSEGV）都被屏蔽，用户态异常永远无法投递 → #GP 死循环。改成标准语义。
11. **SigAction 字段序错**：`#[repr(C)] SigAction` 是 handler/mask/flags/
    restorer，而 x86_64 `struct sigaction` 是 handler/flags/restorer/mask，
    内核把 sa_flags 当 mask 读、sa_restorer 当 flags 读。改字段序。
12. **栈堆重叠**：栈被放在 `max_va+0x10000`（数据段之后），而 brk 堆也从
    那里向上长；malloc sbrk 把堆顶进栈区，setup_frame 往栈上写的信号蹦床
    （`mov $15,%rax; syscall`）砸进 malloc 的 WORD_LIST.next，变成 glibc
    读到「代码字节」的野指针 → `ls` SIGSEGV。栈移到 0x7FFF_FF00_0000 高位。

另：brk 失败改返回 `-ENOMEM`（原来返回 old_brk 正地址，glibc 判失败失效）；
mmap file-backed ENOMEM 回滚已映射页（原来泄漏）。

**验证**：glibc 动态 bash 跑 `echo HELLO; ls -la /; echo LS_OK; echo BASH_DONE`
全通过、exit 0；t1_dyn(t1_static) fork+malloc 正常；默认 selftest boot ok。

**剩余已知问题（未解决）**：
- **ext2/ext4 写路径已修复（2026-08-17 晚）**。mke2fs -b 4096 的文件系统块
  与固定 1024 字节缓冲缓存的缩放 bug 已全部修完（bug-050..055）：
  (a) bitmap 位图块号双重缩放（gd.*_bitmap 已被 read_super_full 缩放，又乘
  scale）；(b) alloc_block 返回 fs 块号但调用方当 1024 块号用、write_inode 把
  i_block 写成 u16 2 字节而非 u32 4 字节（统一约定 i.data[]=fs 块号、bmap 按
  sub 块换算、write_inode 按 u32 写回）；(c) add_entry 新块 rec_len 写错且不
  清零；(d) create 不设 S_IFREG；(e) read_inode 只读 i_block[0] 丢 1..8 块；
  (f) 空闲计数不回写（新增 ext2 write_super + s_dirt）、unlink/rmdir 泄漏
  inode、mkdir/rmdir 不改 nlink/used_dirs_count。
  验证：init_fs 探针 open/write/reopen/read "hello" 通过；init_fs2 探针
  mkdir + 5000 字节写读回 + 多文件 + unlink + rmdir 全部通过；e2fsck -fn
  五个 pass 全过、零错误零警告。
- **posix_spawn 子进程克隆栈读垃圾（已修复，bug-058/059）**：根因不是
  CLONE_VM 共享页表，而是两层内核栈 bug：
  1. clone/fork 用 `get_free_page()` 只分配 1 页当内核栈，但
     `KERNEL_STACK_SIZE=8192` 是两页；第二页没分配，被父进程缺页
     resolve_reserved 拿去当子栈，clone 写 pt_regs（rsp=child_stack/ss=0x23）
     正好覆盖子栈里的 fn/arg。改为 `sched::alloc_kstack()`（KSTACK 池，4 页
     连续对齐），release 用 `free_kstack()`。
  2. clone 从没设 `(*child).kernel_stack = stack_page`，子进程继承父进程的
     栈基址，release 把父进程(fsinit)的槽位 free 掉，下一次 clone 拿到重叠栈
     → 内核 #UD panic。已在布置 tss.rsp/rsp0 后补上。
  配套（bug-056 保留）：CLONE_VFORK 挂起父进程 + 立刻 schedule、execve/do_exit
  唤醒父进程、execve 不 free 共享的 old_pml4。
  验证：静态 init 连做 3 次 `system("echo ...")` 全部 exit 0；fork 5 次
  waitpid 全回收；ext2 写读回 + e2fsck -fn 五个 pass 干净。
- **ext2 多块组分配已实现**：`alloc_block`/`alloc_inode` 改为跨组遍历
  （`alloc_block_any`/`alloc_inode_any`，先组 0 再读 GDT 里组 1..N 的描述符），
  free 按块号/inode 号反推组。块号 >65535 截断已由 bug-057 修掉（i.data 改
  u32、12 直接块）。已知小缺陷：`write_super` 只回写组 0 的 free 计数到超级块
  （超级块里应是各组之和），组描述符本身是对的。
- lfs3(musl busybox) 的 `/sbin/init` 现在以 PID 1 运行后会继续走到控制台
  初始化，然后在 `rip=0x6f0f4`（低位、低于 0x400000 的首个 LOAD 段）触发
  NX 取指缺页退出。疑似 musl 静态二进制在低地址的某段蹦床（`__restore_rt`
  或类似）未被映射为 USER|EXEC；glibc 路径不受影响。
- 测试根里用的 coreutils 是 cargo 的 uutils 多调用二进制，argv[0] 需要是
  精确 applet 名才选对函数，`ls` 会打出 `<unknown binary name>` 的 usage；
  内核 execve 传 argv 本身是对的（已用 argc/argv 探针验证 argc=3 全对）。
  真实 LFS 用 GNU coreutils 独立二进制，不受影响。
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

### 合并镜像启动修复（2026-08-10）

#### 问题
单盘「合并镜像」（`lfs-docker/build-combined.sh`：shitix.img 占起始 1MB
+ LFS rootfs 紧随其后）启动失败：
- LFS 启动路径硬编码 `mkdev(3,1)`（从盘），单盘环境从盘不存在 →
  `I/O error at sector 2 drive 1` → MINIX superblock 读不出 → `IDE not found`。
- 即使改挂 `mkdev(3,0)`（主盘），rootfs 在 1MB 偏移处（扇区 2048），
  原 hd 驱动读绝对 LBA 不加偏移，会读到引导区而非 ext4 超级块。
- 更隐蔽：探测过「不存在的从盘」后，IDE 控制器选中状态留在从盘（浮空
  0xFF/0x00），随后读主盘时 `hd_read_sector` 先 `ide_wait_ready` 再
  `ide_select_device`——状态寄存器反映的还是上个选中的（不存在的）从盘，
  BSY 永远不清，主盘读全部「not ready」超时。

#### 修复
- **hd 偏移**（`src/drivers/block/hd.rs`）：新增 `HD_OFFSET[dev]` +
  `set_offset()`/`drive_size()`。`do_hd_request` 把文件系统相对 LBA 加偏移
  得到盘上绝对 LBA，并按绝对 LBA 做越界检查。
- **选盘-就绪顺序**（`hd_read_sector`/`hd_write_sector`）：改为先
  `ide_select_device` → `ide_settle()`（读 4 次状态让选盘生效，ATA 规范
  要求写完 Drive/Head 后约 400ns）→ 再 `ide_wait_ready`。修复缺从盘后
  主盘读全部超时的根因。
- **LFS 启动回退**（`src/lib.rs` fs_init_thread）：先试从盘 `mkdev(3,1)`
  （双盘布局）；失败且主盘 >2048 扇区（说明后面有 rootfs）时，给主盘设
  偏移 2048 再试 `mkdev(3,0)`（合并镜像）。纯 1MB 引导镜像不触发回退，
  避免刷「out of range」警告。

#### 验证
- 合并镜像（单盘 shitix-lfs-combined.img）：`VFS: Mounted root` →
  `/sbin/init` 执行（`init: must be run as PID 1`）。
- 双盘（shitix.img + lfs3.img）：无回归，`/sbin/init` 执行。
- 默认引导（单 1MB 镜像）：干净落到 selftest，`boot ok`。

### SMP 真正实现（2026-08-22）

AP 启动从骨架变为真实现：
- `boot/ap_trampoline.S`：16 位实模式→保护模式→long mode 蹦床，链接基址
  固定 0x2000（低 1MB 保留区里唯一空闲的 4KB；0x8000 不可用作跳板——它在
  内核镜像 0x10000.._kernel_end 内部，会被自身覆盖）。页尾 0x2F00 起是信箱
  （magic/CR3/栈顶/Rust 入口/逻辑 CPU 号），BSP 经 PHYS_MAP_BASE 直写。
- `scripts/build.sh` 先汇编蹦床（`ld -Ttext 0x2000 --oformat=binary`），
  Rust 侧 `include_bytes!` 嵌入，smp_init 再拷到 0x2000。
- `smp::smp_init()`：map_range 把 LAPIC MMIO(0xFEE00000, PCD 禁缓存) 映进内核
  PML4（引导 PDPT 第 3 项 3-4GB 空闲，不会撞 2MB 大页）→ 使能 BSP LAPIC →
  fw_cfg(0x510/0x511) 读真实 vCPU 数 → 按 INIT-SIPI-SIPI 逐核启动。
- AP 入口 `smp_ap_main`：`desc::ap_load_tables()`（lgdt+lidt 共享表，**不 ltr**——
  TSS 被 BSP 独占且已 busy，再 ltr 会 #GP；CS 用 lretq 从蹦床 GDT 换回
  KERNEL_CS）→ 本核 LAPIC 使能、屏蔽 LINT/Timer → 置 CPU_ONLINE → 进入
  派工等待循环（`run_on_all_cpus`：JOB_SEQ 代序号 + 每核 JOB_DONE 回执，
  BSP 自己也跑一份 cpu0）。
- AP 栈：每核 4 物理页映到 0xffff_8000_4000_0000+（PHYS_MAP_BASE+1GB 之上，
  只在内核 PML4），32KB 步进含 guard 洞。
- 验证：-smp 1/2/4/8 全部 SHITIX_BOOT_OK；多核并行求和与单核参考值一致。

踩过的坑：
- **等待必须用 jiffies 不能数 pause**：TCG 宿主限速下忙等待比真实时间快几十倍，
  AP 的 vCPU 线程拿不到时间片，SIPI 永远等不到回应。
- **不要轮询 ICR delivery status (bit12)**：QEMU 下对不存在的目标永不清位。
- **不能对不存在的 APIC ID 发 INIT**（level-assert 会把 TCG 的 ICR 写卡死）——
  用 fw_cfg NB_CPUS 精确枚举。
- 调度器仍是单核：AP 不参与调度，只做显式派工的并行计算。

### 已知未解决问题
- ~~ls/cat #GP（glibc malloc 腐败 chunk）~~ —— 见「本次会话修复」第 12 条：
  根因是栈堆重叠（信号蹦床字节砸进 malloc 的链表指针），栈移到高位后已修。

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

### 本次会话修复（e1000/UDP 接线 + PCI BAR 修复，2026-08-22）

- **PCI BAR 读取偏移修复**：`pci::get_bar()` 从配置空间 `0x10` 起读 BAR0（原来误从 `0x04` 命令寄存器读），e1000 probe 能拿到 MMIO BAR 了
- **e1000 MMIO 页表映射**：MMIO 在 3-4GB 高位，引导页表只恒等映射低 1GB；e1000 init 里用 `paging::map_range()` 把 MMIO 映进内核 PML4（PCD|PWT 禁缓存，同 LAPIC），解决 `page fault: 0000 CR2=0xfebc0000`
- **UDP 收包分发**：netif 收到 IP_PROTO_UDP 时调用 `socket::udp_input()` 按目的端口分发到 sock entry
- **AF_INET socket 接线**：`sys_bind/connect/sendto/recvfrom` 扩展支持 UDP over e1000（SOCK_DGRAM 走 netif send_ip_packet）
- **SYS_CALL_TABLE 运行时填充**：静态数组（363×8=2904B .data）改运行时填充（BSS），
  省 4KB 静态区，保 0x90000 安全区；extra-drivers + ext4 EXT4_INFO 置零后 release
  `_image_end=0x8F600 < 0x90000`
- **串口 RX IRQ 已存在**：`irq::request_irq(4, serial::irq_rx_handler)` 在 `char_dev::init` 里，
  不再需要轮询兜底（轮询代码保留）
- 验证：`-smp 4` 全自检通过、e1000 selftest 发包 OK、netif/UDP 自检 OK

### 剩余系统调用缺口（2026-08-22 盘点）
- **总 nr 常量 363 个，已实现 361 个，剩余 0 个 ENOSYS**（2 个是 UNUSED/NR_SYSCALLS 占位符）
- io_uring 保持 `-ENOSYS` stub（合理：完整实现需 mm 环队列 + 异步调度器）
- LFS 镜像文件（lfs3.img/gnu-full.img）是 git-lfs 指针，需要 `git lfs pull` 或可用 token

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

### 本次会话（2026-08-22，第二批 syscall + 镜像布局/ext4 修复）

#### 第二批 syscall（src/syscall/sys.rs，均已接线 nr 表）
- `close_range`(436)、`chroot`(161，魔法数/per-task root)、`reboot`(169，魔法数校验+
  键盘控制器复位/HALT/POWER_OFF)、`capget`/`capset`(125/126，单用户 uid0 模型，v3/v1
  布局)、`syslog`(103，type 2/3/4/10，依赖 printk 日志环；printk.rs 新增 `clear_log()`、
  `LOG_BUF_LEN` 改 pub)、`rt_sigpending`(127)、`rt_sigsuspend`(130)、
  `rt_sigqueueinfo`/`rt_tgsigqueueinfo`(129/297，共用 sigqueue_common)、
  `sched_setattr`/`sched_getattr`(314/315，SCHED_OTHER 单策略)、
  `recvmmsg`/`sendmmsg`(299/307，逐条走 recvmsg/sendmsg)。
- chroot 配套：`Task.root` per-task 根 inode；namei.rs 5 处根解析点改走
  `super_block::task_root_inode()`（范本：`pwd_inode()`）；close 抽出 `close_one_fd()`。
- 验证：syscall_selftest 第二批全过（注意 task[0] pid=0，rt_sigqueueinfo 拒 pid<=0
  属正确行为，测试里改用 kill(0,SIGUSR1) 投递）；busybox `chroot / /bin/echo`、
  `chroot /nonexistent`（ENOENT）、`chroot /tmp/jail`（换根后 exec ENOENT）全对。

#### kernel 镜像布局修复（boot/kernel.ld）
- `_kernel_end`（含 BSS）超过 0x9F000 ASSERT：SMP 提交后 release+extra-drivers
  和 debug 全都链不上。**BSS 挪到 1MB**（objcopy -R .bss 本就不进装载镜像，
  head.S 上电清零）：装载镜像（text/rodata/data）ASSERT ≤ 0x90000（bootsect
  线性加载到 0x10000，0x90200 是 setup），BSS ASSERT ≤ 0x200000
  （后面是 mem_map/引用计数表/内核栈池）。
- dev profile 改 `opt-level="z"` + 关 debug-assertions/overflow-checks，
  debug 镜像才塞回 512KB 装载窗口（约 0x83EA0）。
- 注意：BSS 旧位置 0x83000+ 本来就盖 0x90000 参数区（head.S 先存寄存器再清
  再恢复）；挪走以后这段保护变成多余但无害。

#### ext4 read_inode：extent 判定看 magic 不看 flag
- 症状：lfs3.img 挂载成功但整盘 ENOENT（/init、/bin、/sbin 全 -2）。
- 根因：仓库构建脚本生成的镜像 inode 带 EXT4_EXTENTS_FL 但 i_block 区是
  经典块指针（无 0xF30A 头）。read_inode 按 flag 走 extent 分支 → i.data 全 0
  → 根目录不可读。改成 `uses_extent() && magic==0xF30A` 才算 extent
  （bmap_phys 本来就按 magic 判，两处口径现在一致）。

#### 环境/工具备忘
- 仓库里 `*.img` 是 Git LFS 指针（133 字节）；`git checkout` 会把真镜像换回指针。
  没有 git-lfs 时用：curl -L https://media.githubusercontent.com/media/stexiang/shitix/main/lfs3.img
  （oid sha256=45b91886…, 64MB）。真镜像备份在 /tmp/lfs3.img。
- hd_identify 选盘后补了 ide_settle()（ATA 400ns），与读/写路径一致。
- 串口喂 shell 命令要先 sleep 6 等 shell 起来，否则开头字符被吃掉。

### VM/swap 批次（2026-08-22，分支 feat/vm-swap-mm）

- **pipe 的 select/poll 真就绪**：pipe 带环形缓冲状态，select/poll 走
  pipe_poll 查可读/可写，不再「fd 存在即就绪」。
- **vmalloc**：`src/mm/vmalloc.rs`，高半区 vmalloc 区按页映射 + vfree，
  自检 `vmalloc: selftest -> ok`。
- **惰性 file-backed mmap + VMA 记账**（`src/mm/mmap_vma.rs`）：
  mmap(file,PRIVATE) 只建 RESERVED 叶子 + 记 VMA(sb,ino,offset)，首次
  缺页 `resolve_file_fault` 按偏移读文件内容。fork 时 `clone_table`
  复制 VMA 表，exec/exit 清空。每任务 MAX_VMAS=32。
  **坑：glibc ld.so 先整文件 map 一次再逐段 MAP_FIXED 重 map**——
  MAP_FIXED（addr!=0）必须先 `remove_range` 清旧 VMA 再 add，否则
  重叠检查拒成 ENOMEM（"failed to map segment from shared object"）。
- **swap**（`src/mm/swap.rs`）：叶子 PTE PRESENT=0 + SWAPPED(bit10)、
  bits12+ 存槽号；swapon/swapoff 真实现（只接块设备，容量按驱动块数，
  槽位图 + 引用计数，对应原版 swap_duplicate/swap_free）；
  `get_free_page` OOM 时 `reclaim_once` 从当前任务线性扫页表换一个
  干净 RW 用户页出去（重入保护 SWAP_RECLAIMING）；缺页路径
  （traps.rs 用户态+supervisor 两条）先试 `try_swap_in`；fork 的
  cow_copy_page_table 复制 SWAPPED 项并 slot_ref_inc；do_exit
  free_task_swap 归还槽位。自检 `swap: swapon/swapout/swapin/swapoff -> ok`。
- **mprotect 覆盖惰性页**：旧 set_page_flags 对非 PRESENT 叶子直接
  返回 false，glibc 对未触碰 RELRO 段的 mprotect 静默丢失；现在
  RESERVED/SWAPPED 叶子走 leaf_entry/set_leaf_entry 只改 prot 位，
  整段覆盖的 VMA 同步 set_prot。
- **mincore 真实现**（原来不写 vec 直接返回 0）；**mremap 真实现**
  （MAYMOVE 整体搬叶子项不拷物理页，VMA drain_range 平移重挂）。
- 页表新接口：`paging::leaf_entry`/`set_leaf_entry`（读写非 PRESENT
  叶子原值），flags 新增 `RESERVED=1<<9`/`SWAPPED=1<<10`。
- 调试教训：build 脚本输出重定向到 /dev/null 后编译错误不可见，
  会拿旧镜像空跑——仪器化排查时务必先确认镜像时间戳。
- 验证：debug selftest 全过（SHITIX_BOOT_OK）；glibc bash（gnu-full.img）
  echo/ls -la/cat 全通；busybox lfs3 无回归（喂命令要先 sleep 等提示符）。

### 本次会话修复（Batch E/F/G + SMP CI，2026-08-22）

- **Batch E**（fad9229）：POSIX 消息队列（`src/fs/mqueue.rs`，mq_open/
  timedsend/timedreceive/unlink/notify/getsetattr，mqd_t 魔数编码）+
  pidfd_open/send_signal/getfd（PIDFD_MAGIC=0x5046_4400 编码 fd）。
- **Batch F**（cc7d4bf）：eventfd/timerfd/signalfd/inotify 真实阻塞
  （O_NONBLOCK 返回 EAGAIN，否则 schedule() 让出循环——无 waitqueue，
  注意自检里「空读」必须用 NONBLOCK，否则单任务死循环）；epoll 兴趣表 +
  真实就绪查询（eventfd 计数/timerfd 到期/inotify 队列/pipe poll_status），
  不再「fd 存在即就绪」；inotify watch 表 + 事件队列，VFS 钩子
  （open O_CREAT/unlink/mkdir/rmdir/rename → notify_path_event）。
- **镜像空间教训**：0x90000 安全区 = 512KB 镜像上限。NIL=usize::MAX
  编码的 per-task fd 表会把整表烧进 .data：TASK_FILP(32KB)+
  PIPE_FD_MAP(16KB)+event FD_MAP(16KB)=64KB。已统一改成 **0=空、
  存下标+1**（全 0 初始化落 BSS），syssize 32676→28725 clicks。
  以后新静态大表务必 0 哨兵。
- **SMP**：ap_trampoline.S + INIT-SIPI-SIPI 早已可用，缺的只是 QEMU
  没给多核。test.sh 现默认 `-smp 4`（SMP=1 退回单核）：4 CPU online、
  run_on_all_cpus 并行求和与单核参考值一致。调度器仍只在 BSP 上跑，
  AP 仅响应派工。
- **Batch G**（fea7aa6）：umount2（清 i_mount + iput 被挂根）、getcpu
  写回（恒 0/0，调度只在 BSP）。reboot/chroot/shebang 此前已实现。
- **push 被拒**：GITHUB_TOKEN 属 alphashit，对 stexiang/shitix 无写权限
  （403），提交只到本地 feat/vm-swap-mm 分支，待用户换凭据后 push。
