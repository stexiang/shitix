<!-- Last updated: 2026-08-08 (e1000 driver complete with ARP selftest; CLONE_SIGHAND; SA_SIGINFO sigframe; POSIX timers/SysV IPC/sigaltstack; PCI port IO fix; all nonblocking gaps closed) -->
# STATUS — shitix

> Single source of truth for resuming work. Read this FIRST when starting a session.
> Update this file at the end of every work phase so the next `/clear` resumes in 1 read.

---

## ✅ Done

**引导链（64 位）**
- `bootsect.S` 512B 引导扇区，INT 13h/42h LBA 读盘，自搬到 0x9000
- `setup.S` 机器参数 + E820 探测、开 A20、建 4 级页表（恒等映射低 1GB / 2MB 大页）、切 long mode
- `head.S` 装 GDT/IDT（256 门 → `ignore_int`）、16KB 内核栈、清 BSS、`call start_kernel`

**模块 1：内核入口 + VGA 文本输出**（2026-08-02）
- `src/lib.rs`：`start_kernel(*const BootParams) -> !`，打印 e820 条目数 / 可用 RAM / 显示模式 / 光标位置；`#[panic_handler]` 白字红底 + 位置信息 + 串口 `SHITIX_PANIC`
- `src/console.rs`：`Color`(16 色) / `ColorCode` / `Writer`(impl `fmt::Write`)，滚屏、`\n\r\t\b`、0x3D4/0x3D5 硬件光标；导出 `print!` `println!` `cprint!` `cprintln!`
- 验证：debug + release 均 PASS；`screendump` 确认 7 行文本、4 种颜色（cyan/gray/green/yellow）、光标下划线、panic 红底渲染正常

**模块 2：内存管理**（2026-08-02，对应原版 `mm/`）
- `src/mm/page.rs`：`PAGE_*` 常量、`page_align`、`map_nr`（原版 `page.h`）
- `src/mm/page_alloc.rs`：页帧分配器（原版 `mem_init` + `__get_free_page`/`free_page`）。`mem_map` u16 引用计数 + `MAP_PAGE_RESERVED`，空闲链表指针存在空闲页头 8 字节；低 1MB 整体保留
- `src/mm/kmalloc.rs`：八档小块分配器（原版 `kmalloc.c`），`PageDescriptor`/`BlockHeader`/`MF_USED`/`MF_FREE`，整页空闲则归还页帧分配器
- `src/mm/paging.rs`：四级页表 `map_page`/`map_range`/`translate`/`unmap_page` + `flags`（原版 `put_page`/`remap_page_range`/`invalidate`），能识别 setup.S 的 2MB 大页
- `src/e820.rs`：把 setup.S 的 E820 表包成迭代器喂给 mm
- `src/serial.rs`：加 `fmt::Write`，新增 `sprint!`/`sprintln!`/`kprintln!`
- 工具链：`rust-toolchain.toml` 固定 nightly，加 `x86_64` 0.15 依赖
- 验证：`mm_selftest()` 三条路径全绿（页分配去重+清零、kmalloc 6 档读写、映射别名+translate+unmap）；临时压力测试确认超大 kmalloc 返回 null、double free 只计一次、保留页释放是 noop、耗尽 65214 页后可全量回收；`-m 32M/128M/1G/3G` 与 release 均 PASS；零警告
- 踩坑：低 1MB 的页表被当空闲页派出去导致三重错误，见 buglog bug-001

**模块 3：内核基础库 `src/klib/`**（2026-08-02，对应原版 `lib/` + `kernel/vsprintf.c` + `kernel/printk.c`）
- `src/klib/ctype.rs`：256 字节分类表 + 11 个 `is*` + `tolower`/`toupper`/`isascii`/`toascii`（原版 `lib/ctype.c`）。全 `const fn`，去掉原版那个非重入的全局 `_ctmp`
- `src/klib/string.rs`：`strlen/strnlen/strcpy/strncpy/strcat/strncat/strcmp/strncmp/strchr/strrchr/strspn/strcspn/strpbrk/strstr/strtok` + `memcpy/memmove/memset/memcmp/memchr` + Rust 侧 `c_str()`/`c_str_bytes()`（原版 `include/linux/string.h` 的 19 个 `extern inline`，手写 386 串指令改成普通 Rust）
- `src/klib/errno.rs`：全部 122 个 errno + 三个内部 `ERESTART*` + `strerror()` + `KResult<T>`/`from_raw`/`to_raw`（原版 `include/linux/errno.h`）
- `src/klib/vsprintf.rs`：`simple_strtoul`/`simple_strtol`/`skip_atoi`/`number`/`number_u64`/`NumFlags`/`Cursor`/`vsprintf`/`sprintf` + `ksprintf!`（原版 `kernel/vsprintf.c`）
- `src/klib/printk.rs`：8 级 `Level`、4KB 环形 `log_buf`、`console_loglevel` 过滤、串口镜像、`read_log()`；宏 `printk!`/`pr!`/`pr_emerg!`..`pr_debug!`（原版 `kernel/printk.c`）
- 验证：`klib_selftest()` 6 组全绿（ctype 边界、string 全函数含 strtok 就地切分与 memmove 重叠、number 七种补位组合、strtoul base 嗅探、vsprintf 截断语义、printk 级别过滤）；debug + release 均 PASS；零警告
- 踩坑：`number()` 直译 `while(size-->0)` 丢了副作用导致补位翻倍，见 buglog bug-002

**模块 4：中断 / 系统调用 / GDT-IDT / 进程调度**（2026-08-02，对应原版 `kernel/traps.c` `irq.c` `sys_call.S` `sched.c` `fork.c` + `include/linux/sched.h`）
- `boot/entry.S`（新）：`SAVE_ALL`/`RESTORE_ALL` 宏、21 个异常桩、16 个 IRQ 桩、`system_call`(int 0x80)、`ret_from_sys_call`（含 bottom-half 与 reschedule 分支）、64 位特有的 `switch_to`/`ret_from_fork`/`kernel_thread_entry`
- `src/desc.rs`：GDT(7 项：NULL + 内核/用户 CS·DS + 16 字节 TSS 描述符) + 单个 TSS(rsp0 + 3 个 IST 栈) + 256 项 IDT + `set_intr_gate`/`set_trap_gate`/`set_system_gate`/`set_trap_gate_ist`（原版 `head.S` 的 gdt 表 + `traps.c:trap_init()` + `asm/system.h` 的 `_set_gate` 宏）
- `src/traps.rs`：`PtRegs`（21 字段，与 entry.S 逐项对应）、`TRAP_INFO` 21 项元信息表、`do_trap` 统一分发、`die_if_kernel`（含 cr2 错误码位展开、寄存器全 dump、栈 dump）、恢复模式开关供自检用
- `src/irq.rs`：8259A 重映射到 0x20-0x2F、`request_irq`/`free_irq`/`disable_irq`/`enable_irq`（含 `cache_21`/`cache_A1` 屏蔽字缓存）、`do_IRQ`（合并原版 do_IRQ + do_fast_IRQ）、bottom half、`cli`/`sti`/`local_irq_save`/`restore_flags`、`intr_count`/`bh_active`/`bh_mask`（被 entry.S 引用）
- `src/sched/task.rs`：裁剪版 `Task`（保留 state/counter/priority/signal/blocked/flags/errno/pid/comm/亲子链/调度环/时间统计/内核栈/Tss），`TaskState` 7 态、`PF_*` 标志、`STACK_MAGIC`
- `src/sched/mod.rs`：`schedule()` 两遍扫描（原版算法逐条对应，含「候选跳过 init_task、重发包括 init_task」的不对称）、`do_timer`、`WaitQueue`(侵入式下标链) + `sleep_on`/`interruptible_sleep_on`/`wake_up`/`wake_up_interruptible`、`kernel_thread`(原版没有)、`switch_to_task`(软件切换 + 改写 TSS.rsp0)、`sched_init`(编程 8253 到 100Hz + 注册 timer IRQ)
- `src/syscall/`：`nr` 表(i386 调用号)、`do_syscall`(含原版的 errno 覆盖与 CF 标志语义)、`SysArgs`(rdi/rsi/rdx/r10/r8/r9)、137 项分发表、9 个已实现的 `sys_*`(exit/getpid/getppid/getpgrp/pause/times/write/uname/idle)、`syscall0`/`syscall3` 供内核态自检
- 验证：三个自检全绿 —— traps(int3/除零/无效opcode 都打印完整现场并恢复继续跑)、syscall(getpid/getppid/越界→-ENOSYS/未实现→-EINVAL/write 到控制台/uname)、sched(timer 21 ticks、两个内核线程各跑 14 轮、42 次上下文切换)；debug + release + `-m 32M/1G` 四轮均 PASS；零警告
- 踩坑五个，全部记入 buglog bug-003..009：GDT 代码段 L&&D 非法组合→三重错误；`asm!` 的 nomem/nostack 谎报；jiffies/TRAP_COUNT 非 volatile 被提升；schedule 候选扫描没跳过 task[0]；do_timer 跳过 task[0] 导致 need_resched 永不置位

**模块 8：ext4 磁盘结构解析器 + 51 项自检**（2026-08-07）
- `src/fs/ext4/` 共 1291 行：超级块、GroupDesc（32B/64B）、inode、ExtentHeader/Idx/Extent、DirIter 等结构体及访问器
- `src/fs/ext4/selftest.rs`：51 项覆盖超级块字段、GroupDesc 32B/64B、inode 类型/uid/gid/size、extent lookup（hit/hole/mid/unwritten/48bit phys）、DirIter、corruption dir、ino_to_group/disk；零 `format_args!` 节约约 35KB rodata
- `src/fs/ext4/super_block.rs`：加 `from_slice(&[u8])` 变体，selftest 用 128 字节缓冲而不是 1024
- 验证：QEMU `ext4: selftest 51/51 all ok`，`_kernel_end = 0x8A4F0`（23KB 余量）
- 踩坑：`kprintln!` 双份 `format_args!` 让 selftest 对象涨 5× → 撞 ASSERT（详见 cerebrum 2026-08-07 条目）；block_bitmap_hi 字节偏移写错（bug-030）

**Stage 2：进程生命周期——signal / exit / fork / wait4**（2026-08-07）
- 接受标准全部达标（见 `exit::fork_selftest` 端到端）：
  1. ✅ `send_sig(SIGSEGV, ...)` 真正投递：用户态异常不再只打日志，`traps.rs` 直接调 `signal::send_sig`，`entry.S:ret_from_sys_call` 钩子接 `do_signal` 在返回用户态前执行
  2. ✅ `do_exit` 完整流程：close_all → reparent_children → state = Zombie → notify_parent(SIGCHLD + wake_up_waiter) → schedule()
  3. ✅ `sys_fork` 真正复制：pt_regs 21 qwords 复制到子进程内核栈（rax=0），switch_to 帧（7 slots），挂进调度环，父返 child_pid 子返 0
  4. ✅ `ret_from_sys_call` 接 `do_signal`：`signal_pending_c` 谓词 → `do_signal(regs)` 循环消费信号（SIG_DFL/SIG_IGN/自定义→terminate）
- 变更文件：
  | Type | File | What |
  |------|------|------|
  | edit | `src/signal.rs` | per-task sigaction 表（按需分页，128B 指针数组→get_free_page）、`do_signal`（SIG_DFL/SIG_IGN/STOP/CONT 全分支）、`send_sig` 补原版 `generate()` 过滤（SIG_DFL 默认忽略的不置位）|
  | edit | `src/exit.rs` | `do_exit` 补齐（close_all + reparent_children + 正确退出码/信号编码）、`release` 补 `free_page`/`reset_sigactions`、`sys_wait4` 完整体（四种 pid 语义 + WNOHANG/WUNTRACED + Interruptible 睡眠 + 收尸 release）|
  | edit | `src/syscall/sys.rs` | `fork` 重写（irq_save 临界区 + 栈魔数 + pt_regs 21 qwords 复制 + rax=0 + switch_to 帧 + 调度环插入）、`wait4` 接到 `exit::sys_wait4` |
  | edit | `boot/entry.S` | `ret_from_sys_call` 插 `do_signal` 钩子（`call signal_pending_c` → `call do_signal` → 重回 `ret_check_resched`），在 CS 检查之后、need_resched 之前 |
  | edit | `src/traps.rs` | `send_sig_stub`→真 `signal::send_sig`，swapper 保护（task[0] 收致命信号直接 panic）|
  | edit | `src/lib.rs` | `fs_init_thread` 末尾挂 `exit::fork_selftest`（fork+wait4 端到端）|
- 信号位号约定统一：位号 == 信号号，bit 0 空着（以前 mask/blocked 用 sig-1、send_sig 用 sig，错开 1 位→屏蔽字形同虚设，见 bug-032）
- sigaction 表按需分页：避免 16KB 静态 BSS（见 bug-033），只存 `[usize; NR_TASKS]` 128 字节指针，首次 `set_task_signal` 才 `get_free_page`，`release`/`reset` 时 `free_page`
- 验证：全部自检通过（traps/syscall/sched/fs/syscall-fs/ext4 51/51/fork），`_kernel_end = 0x8C570`（14.6KB 余量），`SHITIX_BOOT_OK`
- 未做（留给后续阶段）：用户态栈上的信号帧（`setup_frame`/`sa_restorer`/`sigreturn`）、`copy_page_tables`（所有任务仍共用内核页表，tss.cr3=0）、PER-CS 的内核态跳过 do_signal（已正确实现，但当前没有 PER-CS 任务所以测不到这条分支）
- 踩坑三个：bug-031（send_sig 漏 generate 过滤→notify_parent 的 SIGCHLD 让 wait4 被自己孩子打断）、bug-032（signal/blocked 位号约定不一致→屏蔽字形同虚设）、bug-033（per-task sigaction 16KB 静态数组吃光 BSS 余量→按需分页）
- Key learnings 六条进 cerebrum：信号位号约定、send_sig 过滤、_kernel_end 硬约束、内核态 fork 的子进程现场、do_signal 钩子的内核态跳过、汇编不写死 Rust 结构体偏移

**Stage 3：用户态 ring-3 切换**（2026-08-07）
- 接受标准全部达标：
  1. ✅ `iretq` 到 USER_CS(DPL=3)、执行用户代码（getpid→exit(42)）、int 0x80 回来
  2. ✅ fork + per-task PML4：子进程有独立页表（共享内核 PDPT[0]，独立 user PDPT[1]），父 wait4 收尸
  3. ✅ `copy_from_user`/`copy_to_user` + `verify_area`：新增 `src/mm/area.rs`（逐级查 USER 位），保留 `check_range` 兼容未迁移 syscall
  4. ✅ 用户态缺页→信号路径：page_fault handler 已接 COW（umm::try_handle_cow_fault），非 COW→send_sig→do_signal→do_exit
- 变更文件：
  | Type | File | What |
  |------|------|------|
  | edit | `src/mm/paging.rs` | 修 copy_page_table bug（entry 首参数用错）；修复 next_level 缺「对已存在条目追加 USER」→ bug-034；alloc_pml4 + clone_kernel_pdpt；index/entry 函数改 pub |
  | **new** | `src/mm/area.rs` | verify_area、copy_from_user、copy_to_user、strncpy_from_user（逐页 translate + 恒等映射拷贝） |
  | edit | `src/umm/mod.rs` | create_user_process（分配 PML4+代码页+栈页，用 map_page 建映射） |
  | edit | `src/sched/task.rs` | Task 加 pml4: usize（0=共享内核页表） |
  | edit | `src/sched/mod.rs` | switch_to_task 纠正：next_cr3==0 时切回 0x4000（bug-035）；kernel_thread 用 pml4 替代 tss.cr3 |
  | edit | `src/syscall/sys.rs` | fork 用 pml4；sys_exit→do_exit（bug-036） |
  | edit | `src/exit.rs` | pml4 释放移到 release()（bug-035）；do_exit 删 debug_assert |
  | edit | `src/traps.rs` | page_fault 用 pml4 字段，COW 条件补 is_user |
  | edit | `src/lib.rs` | user_mode_selftest：fork→launch_user_task→wait4，验证 ring-3 往返 |
- 踩坑四个（bug-034..037）：next_level 漏 USER 位追加、CR3 从用户切回内核不写、sys_exit 不通知父进程、encode_status 误判退出码为信号
- `_kernel_end = 0x8D570`（10.6KB 余量），SHITIX_BOOT_OK，全部自检绿色

---


22 个新文件，约 5000 行：缓冲缓存（`fs/buffer.c`）、块请求队列
（`ll_rw_blk.c`）、ramdisk、tty/console/keyboard/mem 字符设备、
VFS（inode/file_table/super_block/devices/namei/open/read_write/stat）、
完整的 minix v1 文件系统（含原版没有的 `mkfs`）。零编译告警。

内核能 mkfs → 挂载 minix 根文件系统 → 通过 8 项 fs 自检
（块 I/O、挂载状态、readdir、文件读写往返、9KB 间接块文件、
mkdir/rmdir/unlink + 位图回收、/dev/zero、缓冲去重检查）。
debug 与 release 都过，`-m 32M/128M/1G/3G` 都过。

本阶段修掉的 bug（详见 buglog bug-011..022）：
- BSS 盖掉 0x70000 的四级页表 → 三重错误（页表搬到 0x4000，并在
  kernel.ld 加 `ASSERT(_kernel_end <= 0x90000)`）
- 在 task[0] 里跑 mount_root/fs 自检（fs 全路径都会睡，sleep_on 对
  task[0] 是 panic）→ 改成内核线程 `fs_init_thread`
- 内核线程栈一页不够 x86_64 的 fs 调用链 → 静态池 4 页
- `make_request` 与 `wait_on_buffer` 的丢失唤醒（原版靠 cli 罩住 /
  先挂队列再判条件）
- `add_request` 没关中断操作请求队列
- printk / console 输出没有临界区（原版用 cli/restore_flags）——
  这是上阶段 STATUS 里记的已知缺口，本阶段补上
- panic 信息只上 VGA 不上串口；`as_str()` 丢掉带格式的 assert 消息

**模块 6：系统调用号补齐到 x86_64 正式表**（2026-08-06）
- `nr` 模块原本按功能分组手写号，重号一大把：`GETPID`=`WAIT4`=61、`GETPPID`=`KILL`=62、六个 `IO_*` 全是 0（把 `t[READ]` 也覆盖了）。建表是顺序赋值，重号**静默覆盖**，编译器零警告。表现是 syscall 自检一直 `FAIL`（getppid 实际跑的是 sys_kill），此前被当成噪声
- 改成从 `arch/x86/entry/syscalls/syscall_64.tbl` 逐号生成 `0..=334` 连续常量，之后补 io_uring(425-427)/pidfd_open(434)/clone3(435)/faccessat2(439)/epoll_pwait2(441)；自检私有号在 500 段（`IDLE=500`、`UNUSED=501`）。保留 `UMOUNT`/`PRLIMIT`/`SETMEMPOLICY` 等旧名作别名
- 分发表改按号顺序逐项赋值，**341 个槽挂了实现**（原先看似 200 多项，实际有效的远少于此）
- `src/syscall/sys.rs` 新增 133 个实现：
  - 真做：`lseek`(转 fs 层)、`readv`/`writev`(拆成逐 iovec 调 read/write，短读即停)、`sched_yield`、`gettid`、`time`、`exit_group`、`tkill`/`tgkill`、`getpgid`/`getsid`、`getresuid`/`getresgid`、`sched_getaffinity`
  - 合理默认：`madvise`/`mincore`/`readahead`/`fadvise64` 忽略即合法、`fdatasync`→`fsync`、`flock` 单进程无竞争、`getgroups` 返回 0、`utime` 系列忽略（无 RTC）、`sched_get*` 汇报 SCHED_OTHER
  - 占位 `-ENOSYS` 89 个，每个文档注明**缺哪个子系统**（rt_sig* 缺信号栈帧、timer_* 缺 POSIX 定时器池、futex 缺 per-address 等待队列、mq_* 只有 SysV 队列、clone 缺 flags 语义…）
- `implemented_count()` 用 `static WIRED: [bool; 512]` 位图而非比函数指针：release 下 LLVM 的 identical code folding 把返回 `-EINVAL` 的占位实现和 `ni_syscall` 折叠成同一地址，比指针会漏数（debug 341 / release 340）
- 验证：syscall 自检**从 FAIL 转 ok**（`getpid=0 getppid=0 bad=-38 ni=-22`）；debug 6 轮 5 过、release 过、`-m 32M/128M/1G/3G` 过；`sys.rs` 新增段零编译告警
- 踩坑三个，记入 buglog **bug-026..028**；另有一条 Do-Not-Repeat：不要用 `git stash` 做基线对比（这个树的改动都未提交）

**COW 引用计数表改成动态划分**（2026-08-06，修 `ld: ... overlaps setup parameter area at 0x90000`）
- `page_ref.rs` 原本按最大内存开 `[AtomicU32; 65536]` = 256KB BSS，`_kernel_end` 冲到 0xCC2B0，被 `kernel.ld` 的 ASSERT 拦下（该 ASSERT 第一次真正挡住事故）。另外 `page_ref::init()` 从来没被调用过，这 256KB 是白付的
- 改成沿用 `mem_map` 的约定：`page_alloc::init` 在 `mem_map` 之后按实际内存划出 `nr_pages * 4` 字节、清零、调 `page_ref::attach()`，并把 `map_end` 抬到表尾之后确保不进空闲链表；`MemInfo` 加 `page_ref_addr` / `nr_pages`
- `page_ref.rs` 用 `REF_BASE`/`REF_LEN` + `slot()` 取槽，越界返 `None` 不 panic（COW 路径会传来受管内存之外的 pfn）
- 顺手修：COW 标记位从 bit15 挪到 bit31 —— 原本与 `& 0xFFFF` 的计数字段重叠，引用计数到 32768 会被误读成 COW 页
- 验证：BSS 365KB→103KB，`_kernel_end`=0x8C2C0。详见 buglog **bug-024**
- ⚠️ **`33667fd`（COW）之后内核一直没链接成功过**，所以 `33667fd` 和 `5a3c04e` 两个提交的代码在此之前从未运行。最后一个可构建的提交是 `HEAD~2`。链接修好后这两个提交的代码是第一次真正执行，冒出来的问题要按「新代码」看待，别默认归给既有 bug

**修 `SUPER_AREA` 护栏误报**（2026-08-06，buglog **bug-025**）
- 症状：`KERNEL PANIC: SUPER_AREA guard lo smashed at check_mounted`，而 `pr_warn` 打印的值 `0x1234abcd1234abcd` 恰好就是正确的 `SB_GUARD`
- 判据：一次 `read_volatile` 的结果同时喂比较和打印；打印值正确而比较说不等 ⟹ 比较错了，内存是好的
- 根因：`check_guards` 用 `&*addr_of!(SUPER_AREA)` 建了覆盖整个结构（含 `table`）的共享引用，与 fs 路径上存活的 `&mut SuperBlock` 重叠 = UB。`super_block.rs:268` 早写了这个症状，但只针对 `sb()` 那条路径，漏了 `check_mounted`
- 为什么现在才冒出来：见上，新代码首次执行改变了内联上下文，让一直潜伏的误报显形。**HEAD~2 基线跑 20 次护栏 0 次触发**，与「上次测护栏 100% 正常」一致
- 修法：`check_guards` 全程裸指针，不建任何覆盖 `SUPER_AREA` 的引用；失配先复读，复读正确就 warn + continue，复读仍错才 panic（真损坏保持 fail-stop）。修完 20 次 0 次触发
- ⚠️ 未能在修复后复现（裸指针版 20 次 + 保留 UB 只加复读的诊断版 20 次，均 0 次），机制是推断而非直接测到
- ⚠️ **BSS 只剩约 15KB 余量**，下一个加静态缓冲的模块会再撞 ASSERT。放血点：`desc.rs` 三个 8KB IST 栈（24KB）、`sched::KSTACKS`（48KB）

**模块 7：系统调用接到 fs 层 + 内核栈池移出 BSS**（2026-08-06）
- **21 个系统调用从 `-ENOSYS` 占位改成真接 fs 层**：`open`/`creat`/`close`/`read`、`dup`/`dup2`、`chdir`/`chmod`/`truncate`、`mkdir`/`rmdir`/`unlink`/`link`/`mknod`(→`fs::namei::do_*`)、`fsync`、`stat`/`lstat`/`fstat`(→`fs::stat::*`)、`getdents`/`getdents64`。fs 层一直是好的，只是没接上
- `write` 改成 fs 优先 + 控制台兜底：先走 `fs::read_write::write`，只有 fd 1/2 拿到 `-EBADF` 才退回内核控制台（task[0] 没有 stdout/stderr）
- 用户指针护栏 `check_range`/`user_path`/`user_buf`/`user_buf_mut`/`user_stat_out`：只接受恒等映射低 1GB 内的地址，路径按 PATH_MAX 4096 有界 strnlen。⚠️ **它不阻止用户态读写内核内存**，只挡未映射地址；等 `mm/mmap.c` 的 `vm_area_struct` 移植完必须换成真的 `verify_area`
- 新增 `syscall_fs_selftest()`（9 组，全走真 `int 0x80`）：creat+write、lseek+read 内容比对、fstat st_size、dup 相异、close×2 + 二次 close 得 `-EBADF`、按路径 stat、mkdir/rmdir 往返、unlink + stat 失败、EFAULT 护栏（坏指针 + NULL）。必须挂在 `fs_init_thread` 里，**不能在 task[0]**（所有 fs 路径都可能睡）
- **内核栈池移出 BSS**：`.text`/`.rodata` 涨了约 24KB 后 `_kernel_end` 冲到 0x922C0，超 0x90000 共 8896 字节。`sched::KSTACKS`（48KB，BSS 最大项）改成由 `page_alloc::init` 在 `page_ref` 表之后划出、清零、调 `sched::attach_kstacks()`（照 bug-024 的 `page_ref::attach()` 套路），`map_end` 抬到池尾之后确保不进空闲链表；`MemInfo` 加 `kstack_addr`
- 顺带解掉一个长期限制：`KSTACK_SLOTS` **3 → 8**。原注释写明「再加一份就会越界」，现在池子不占 BSS，内核线程数不再被 BSS 卡着
- 修 `write` 自检的过期期望：`write(0,…)` 现在返 `-EBADF`（POSIX 语义，未打开的 fd），此前只认 fd 1/2 直写控制台所以期望的是 `-EINVAL`
- `scripts/check-syscall-nr.py`（新，可执行）：解析 `nr` 模块的 `pub const X: usize = N` 对比内核头的 `#define __NR_x N`，自动找 `/usr/src/linux-headers-*/`，带 `PRIVATE` 白名单和 `sysctl`→`_sysctl` 拼写映射，另自查重号。输出 `官方 360 个号，本树 360 个 / 全部一致`，exit 0
- `scripts/test.sh` 加 `MEM=` 覆盖（默认 256M），照 `TIMEOUT`/`PROFILE` 的既有约定，用来跑内存矩阵
- 验证：`_kernel_end` = **0x854F0**（debug）/ 0x5B4E0（release），距 0x90000 约 44KB 余量，`kernel.ld` 的 ASSERT 原样保留（量溢出时临时放宽过，已完全还原并核对与 HEAD 一致）；debug 与 release **都报 361 wired**（`WIRED` 位图消掉了 ICF 分歧）；`-m 32M/128M/1G/3G` 全部启动且 `syscall-fs` 绿
- ⚠️ **HEAD 基线不可用**：另开 worktree 跑 `HEAD` 会撞同一条 `overlaps setup parameter area` —— 这个树里未提交的 `page_alloc.rs`/`page_ref.rs` 改动**就是**那个修复。别拿 HEAD 做基线对比

## 🚀 Next phase

**Stage 2–8 + 所有非阻塞缺口已关闭。**
下一阶段：**LFS 用户态真实启动 + TCP 协议栈接 e1000**。

### Objective
在 LFS 根文件系统上启动 /bin/bash，实现 TCP/IP 网络通信。

### Scope
1. **LFS 启动测试** — 构建静态 busybox 镜像，`LFS_BOOT=true`，验证 /bin/sh 运行
2. **TCP 协议栈接 e1000** — ARP resolve → IP route → eth_build_header → e1000.send
3. **per-task pwd/root** — chdir 影响隔离

### Objective
把内核从「纯内核态运行」推到真正 iretq 到 ring-3、跑用户代码、再通过 int 0x80（以及 syscall 指令）回到内核。这是 LFS 集成的前提——当前内核从未执行过一条用户态指令。

### Scope
1. **Ring-3 切换基础设施** — 构造用户页表（用户空间 3GB 分割，不使用段基址而是靠页表隔离）、TSS 里填好 rsp0、iretq 到 USER_CS
2. **`copy_page_tables` / `clone_page_tables`** — fork 不再共用内核页表（当前 tss.cr3 == 0），真正给子进程一份**写时复制**的页表
3. **`verify_area` / `copy_from_user` / `copy_to_user`** — 替换当前只挡未映射地址的 `check_range`，对用户态指针做真正的 vm_area_struct 校验
4. **页面错误恢复路径** — page fault 在用户态时不再走 `die_if_kernel`（Stage 2 的 `send_sig_stub` 已经为它留好了信号投递），COW 页的缺页处理也在这里
5. **`syscall` 指令入口启用**（可选）— 目前 `entry.S:syscall_entry` 是 `cli; hlt` 桩，需要 per-task 的用户栈暂存位 + MSR 配置
6. **初始化用户态 init 进程** — fork + iretq 一个最简单的用户态任务跑起来（哪怕只是 `hlt` 循环），验证整套 ring-3 ↔ ring-0 往返

### Files to create / edit
| Type | File | Content |
|------|------|---------|
| edit | `src/mm/paging.rs` | `copy_page_tables`/`clone_page_tables`（COW，fork 时给子进程一份独立的 PML4）|
| edit | `src/syscall/sys.rs` | fork 里接 `copy_page_tables`、补 `sys_execve` 的用户态入口骨架 |
| new | `src/mm/user.rs` | 用户页表构造（map user pages to 0..3GB with USER bit, separate from kernel 1:1 map）、`create_user_process`（见 stage 3 下的具体步骤） |
| edit | `src/mm/page_alloc.rs` | 可能需要加 `get_free_page_for_user`（用户页放在物理地址 > 0x100000 之上） |
| edit | `src/sched/task.rs` | 补 `vm_area_struct`（原版 `mm/mmap.c`，每个 task 的 mm），或新建 `src/mm/vma.rs` |
| edit | `boot/entry.S` | 可能启用 `syscall_entry`（MSR STAR/LSTAR/SFMASK）、加 `iretq` 到用户态后第一次被中断/系统调用回来时确保栈正确 |
| edit | `src/desc.rs` | 确认 USER_DS 的 DPL=3、TSS 的 IST 栈都就绪 |
| New | `src/mm/area.rs` | `verify_area`/`access_ok`（原版 `mm/memory.c` 和 `asm/segment.h`），替换当前 `sys.rs::check_range` |
| New | `src/uspace/` (or in `src/syscall`) | `copy_from_user`/`copy_to_user`/`strncpy_from_user` |

### Open decisions (from Stage 2, mostly still open)
- **`printk` 临界区**：`klib::printk::emit()` 缺中断保护，已知缺口。Stage 3 开始前应该先做。
- **per-task filp/pwd/root**：FD 表目前还是全局的 `FD_TABLE`（`fs/open.rs`），`do_exit` 里的 `close_all()` 是对全局操作的。在 fork 真正分出独立地址空间之前，这项工作对 Stage 3 的正确性更关键了。
- **`arch_prctl`（FS/GS base）**：glibc 启动时调这个装 TLS，execve 之前必须补。
- **x86_64 `struct stat` ABI**：当前的 `Stat` 是 i386 布局，任何返回给用户态的 `fstat`/`stat` 都会被 glibc 误解。Stage 3 应该补 x86_64 版本。
- **ELF64 加载器**：当前只认 ELF32。是否在 Stage 3 一起做、还是留给 Stage 4，看复杂度。
- `syscall` 指令入口是否在 Stage 3 启用待定。

### Acceptance criteria
1. `iretq` 到用户态（USER_CS DPL=3）的一段代码，用户态触发 `int 0x80` 或 page fault → 内核收到并正确处理（信号投递或服务调用），再 iretq 回去
2. `fork` + `copy_page_tables`：父进程写 COW 页触发缺页，拿到自己的私有副本；子进程看到的是 fork 时刻的快照
3. `copy_from_user`/`copy_to_user` 对用户指针做边界检查，写入超出映射范围的地址返回 `-EFAULT`（替换当前 `check_range` 只认恒等映射的假实现）
4. 用户态 segfault（访问 null 或未映射地址）→ `page_fault` → `send_sig(SIGSEGV)` → `do_signal` → `do_exit(SIGSEGV)`，任务被回收（Stage 2 的信号路径与 Stage 3 的缺页恢复挂上）

---

---

## 📁 Active architecture

- **Stack:** Rust `#![no_std]` + edition 2024 + **nightly**（`rust-toolchain.toml` 固定），crate-type = `staticlib`，依赖 `x86_64` 0.15；GNU as + ld；QEMU x86_64
- **Key modules:** `src/lib.rs`(入口) / `src/console.rs`(VGA) / `src/serial.rs`(COM1) / `src/e820.rs` / `src/mm/`(page, page_alloc, kmalloc, paging) / `src/klib/`(ctype, string, errno, vsprintf, printk) / `src/desc.rs`(GDT/TSS/IDT) / `src/traps.rs` / `src/irq.rs` / `src/sched/`(task, mod) / `src/syscall/`(mod=nr 表+分发表, sys=341 个实现) / `boot/*.S`(bootsect, setup, head, **entry**) + `boot/*.ld`
- **陷入/返回 ABI:** `boot/entry.S` 的 `SAVE_ALL` 压栈顺序 == `src/traps.rs` 的 `PtRegs` 字段顺序，改一侧必须同步另一侧。`orig_rax` 格三用途：系统调用号 / 异常错误码 / `!irq`
- **段选择子:** KERNEL_CS=0x08 KERNEL_DS=0x10 USER_CS=0x1B USER_DS=0x23 TSS=0x28（entry.S 里有同名 .set 常量，必须一致）
- **中断向量:** 0-20 异常（2/8/14 走 IST）、0x20-0x2F 是 PIC 重映射后的 IRQ0-15、0x80 是 int 0x80
- **系统调用约定:** 号在 rax，参数 rdi/rsi/rdx/r10/r8/r9（r10 而非 rcx，为兼容 syscall 指令）。**调用号用 x86_64 正式表**，加新号照 `syscall_64.tbl` 逐号填，不要按功能分组手写（重号会静默覆盖，见 bug-026）
- **物理内存约定:** 低 1MB 永久保留（启动期结构）；`mem_map` 放 0x100000 起；可管理内存 clamp 到 1GB（setup.S 的恒等映射上限）
- **Patterns:**
  - 每个 `unsafe` 块上方写 `// SAFETY:`；`unsafe fn` 写 `# Safety` 文档段
  - 新模块的文档注释里注明对应的 linux-1.0.9 原版文件/函数
  - MMIO 一律 `read_volatile` / `write_volatile`
  - C 字符串在裸指针上处理（`klib::string`），`&str` 只在 Rust 内部用
  - 不重复导出 `memcpy`/`memset` 等符号，`compiler_builtins` 已提供
  - 全局可变状态用 `addr_of_mut!` 访问（edition 2024 禁止直接借用 `static mut`）
  - **只在中断/异常里被改的全局量，读取侧必须 `read_volatile`**（jiffies、TRAP_COUNT、跨任务计数器）
  - **会触发异常/中断的 `asm!` 不能加 `nomem`/`nostack`**（那是谎报，见 buglog bug-004）
  - 注释与交流用中文

---

## ⚠️ External blockers (don't block coding)

- 无。工具链（cargo / as / ld / qemu-system-x86_64 / python3+PIL）本机齐备

---

## 🔧 Useful commands

```bash
scripts/test.sh                  # 构建 + 无头启动，匹配 SHITIX_BOOT_OK
scripts/test.sh run              # 带窗口交互运行
scripts/test.sh debug            # 等 GDB: gdb -ex 'target remote :1234' target/boot/system.elf
PROFILE=release scripts/test.sh  # release 构建并测试
scripts/build.sh                 # 只出镜像 target/boot/shitix.img

# 验证 VGA 实际渲染（QEMU 文本模式为 720x400，单元格 9x16）
(sleep 6; echo "screendump /tmp/x.ppm"; sleep 2; echo quit) | \
  qemu-system-x86_64 -drive format=raw,file=target/boot/shitix.img,if=ide \
  -m 256M -no-reboot -display none -serial null -monitor stdio

# 跨内存规格验证（注意：内核 hlt 不退出，必须落盘再 grep，不能直接管道接 grep）
for m in 32M 128M 1G 3G; do
  timeout 12 qemu-system-x86_64 -drive format=raw,file=target/boot/shitix.img,if=ide \
    -m $m -no-reboot -display none -serial file:/tmp/m_$m.log -monitor none >/dev/null 2>&1
  echo "=== $m ==="; grep -aE "^mm: [0-9]|held=|BOOT_OK|PANIC" /tmp/m_$m.log
done

# 查三重错误 / 异常重启
qemu-system-x86_64 ... -d cpu_reset
```

---

## 📚 References (read IF needed)

- `.wolf/cerebrum.md` — User Preferences + Do-Not-Repeat + Decision Log
- `.wolf/anatomy.md` — token-efficient file index
- `.wolf/buglog.json` — known bugs + fixes
