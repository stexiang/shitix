<!-- Last updated: 2026-08-06 (模块 7：21 个 syscall 接 fs 层、内核栈池移出 BSS、LFS 集成结论) -->
# STATUS — shitix

> Single source of truth for resuming work. Read this FIRST when starting a session.
> Update this file at the end of every work phase so the next `/clear` resumes in 1 read.
> Last updated (was): 2026-08-06 (模块 7 完成；syscall 表 361 wired，fs 层通畅，BSS 44KB 余量)

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

---


### 模块 5：文件系统与设备驱动（2026-08-02）
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

**Goal（先做）:** fs 自检 flake 的**真正**根因。这一轮把几个假设读代码排除了，
并拿到一条新线索。

**已排除（读代码，不是猜）:**
- `do_unlink` 每条早退路径都正确取负（`-(e as i64)`），`dir_namei` 的错误是 `u16`
- `minix::namei::unlink` 全部 5 条返回路径都只返正 errno，所以 `-(r as i64)` 是对的
- `Task::errno` 是 `i32`，**全树除了 `do_syscall` 里那句 `= 0` 没人写它**，所以
  `do_syscall` 的 errno 覆盖分支（`regs.rax = -(errno)`）永不触发
- 系统调用号无重号、`UNLINK` 槽位（87）挂的就是 `sys::unlink`（`check-syscall-nr.py` 过）

**探针实测（临时探针已删）:** 失败那次 `unlink=-2, errno 0->0` ——
符号是**对**的，文件确实已经不在了。所以问题不是符号，是文件**提前消失**。

**新线索（下一步从这里查）:** 同一次失败里 `stat st_size FAILED (got 32)`，
而写进去的是 **20** 字节、自检的读缓冲正好是 **32** 字节。`st_size` 取到了
缓冲长度而不是文件长度，指向 inode/缓冲被串写。查 `fs::stat` 填 `st_size`
的路径与 `read` 的缓冲归还顺序。

**⚠️ 与「换 ext4 就能绕开」有关的判断更正:** 这个 flake **大概率不是
minix 独有**。`do_unlink`/`dir_namei`/buffer cache 都在 VFS 层、与文件系统
无关，换 ext4 会一起带过去。而且 ext4 现在**挂不上**（见下），换不了。

**⚠️ 上一版 Next phase 把这个 flake 归给 bug-012/bug-023 是错的**：两条都记录为
已修，且 bug-012 的修法（把 fs 挪进 `fs_init_thread`）正好消掉了它自己写的
「在 task[0] 里睡」这个成因。别再直接套用那两条的结论。

**（旧描述保留作参考）:** fs 自检约 **10-15%**
概率失败（`buf 0 bytes` / 缓冲里出现 BIOS ROM 的 `0xf000ff53` /
`mkfs.minix failed` / mount 找不到魔数 / 漏一个 zone）。详见 buglog **bug-023**。

已排除：页分配器重复派页、缓冲数据页落在低端内存、ramdisk `PAGES`
未初始化、一块两缓冲、空闲环下标越界、内核栈溢出。已确认现象：某个
缓冲头的 `b_size` 变成 0（从没 init 过却挂进了链）。指向仍有一处非
原子的链表/指针更新与中断交错。**下一步**：用 `qemu -d int` 配合在
`add_request`/`end_request`/`getblk` 里记录事件序列（环形缓冲，事后
dump），而不是继续加断言。护栏已就位，不要删。

**这个 bug 与后续所有提交都无关**：`HEAD~2` 基线 20 次失败 2 次，COW/ext4
提交后同样频率，系统调用号补齐后 debug 6 次失败 1 次、32M 首轮失败但同镜像
重跑 3/3 过。每次看到 `mkfs.minix failed` 先归给它，别当成新回归。
护栏 panic（bug-025）不是它的症状，两者不要混。

**Goal（LFS 集成的真实结论 —— 读代码核实过，不是估计）:**
用户问「能否集成进 LFS 系统」。**当前内核跑不了 LFS 用户态**，缺的不是零碎补丁：
- **完全没有 ring-3 切换**。`USER_CS` 只出现在常量定义里，全树没有任何地方
  `iretq` 到用户态 —— 内核**从未执行过一条用户态指令**
- `sys_execve` 是 `-ENOSYS` 占位，函数体里是注释掉的伪代码
- **ELF 加载器只认 32 位**（`parse_elf32`/`Elf32Header`/`parse_phdr32`），LFS 的
  x86_64 二进制是 ELF64
- 没有 `copy_from_user`/`copy_to_user`/`verify_area`/`access_ok`（现在的
  `check_range` 只挡未映射地址，见模块 7）
- `fs::stat::Stat` 是 1.0.9 的 i386 布局（`u16 st_dev`、`u32 st_ino/st_size`），
  **不是** x86_64 glibc 的 `struct stat` —— 对真实用户态是 ABI 不匹配
- `arch_prctl` 是占位（glibc 靠 `ARCH_SET_FS` 装 TLS），且没有 per-task FS/GS base
- `clone` 的 flags 语义、信号投递到用户态、`fork` 的页表复制都缺
- **ext4 挂不上**：`src/fs/ext4/` 共 1291 行，全是磁盘结构解析器（超级块/inode
  字段访问器/extent 结构/特性位查询），**没有** `read_inode`/`lookup`/目录操作/
  块分配器/`mkfs`。`FsType::Ext2` 这个枚举变体除了定义处和 `do_unlink` 里
  一句 `FsType::Ext2 => ENOSYS, // TODO` 之外全树没人用。对比 minix：1250 行
  + 12 处 VFS 分发点，是唯一能挂的磁盘文件系统。
  「换成 ext4」不是配置开关，是一个与刚做完的系统调用工作量相当的独立模块：
  extent 树查找 + 目录操作 + 块组/位图分配 + mkfs，外加把 ramdisk 从 256KB
  扩到几 MB（`mkfs.ext4` 的元数据放不进 256KB）并写一个本树没有的 `rd_load()`
  把宿主造好的镜像搬进 ramdisk 的动态页

**所以下一阶段（然后）:** 移植信号与进程生命周期 —— `kernel/signal.c` 的信号投递/`sigaction`、
`kernel/exit.c` 的 `do_exit`/`sys_waitpid` 收尸链、`kernel/fork.c` 的真正
`sys_fork`。这三者互相咬合，且是把现有调度器接到用户态的前提。

**系统调用侧的接续点已经标好了**：`sys.rs` 里 89 个占位实现每个都注明缺哪个
子系统。信号那批（`rt_sigaction`/`rt_sigprocmask`/`rt_sigreturn`/`rt_sigpending`/
`rt_sigtimedwait`/`rt_sigqueueinfo`/`rt_sigsuspend`/`sigaltstack`）就是这一阶段
要填的；`restart_syscall` 依赖 `ERESTART*` 的回绕逻辑，一并做。
`pselect6`/`ppoll`/`epoll_pwait` 等「带信号屏蔽的等待」也在信号到位后才好转发。

### Acceptance criteria
1. `send_sig(SIGSEGV, ...)` 能真正投递：用户态触发 page fault 后进程被杀而不是只打一行日志
2. `do_exit` 走完整流程：转 `TASK_ZOMBIE` → 通知父进程 → 父进程 `waitpid` 收尸并回收内核栈
3. `sys_fork` 能复制出一个真正的子进程（需要 `copy_page_tables`），父子各自返回不同的 pid
4. `ret_from_sys_call` 里接上 `do_signal`，返回用户态前投递待处理信号

### Files to create / edit
| Type | File | Content |
|---|---|---|
| edit | `src/signal.rs`（已存在，缺用户态栈帧）| `sigaction`/`sigset`/`send_sig`/`do_signal`（原版 `kernel/signal.c`）|
| edit | `src/exit.rs`（已存在，缺收尸链）| `do_exit`/`sys_waitpid`/`notify_parent`/`release`（原版 `kernel/exit.c`）|
| new | `src/fork.rs` | `sys_fork`/`copy_process`（原版 `kernel/fork.c`）；同时把 `sys::clone` 的 flags 语义接上 |
| edit | `src/mm/paging.rs` | 加 `copy_page_tables`/`clone_page_tables`（原版 `mm/memory.c`）|
| edit | `boot/entry.S` | `ret_from_sys_call` 里插 `do_signal` 调用（原版 `signal_return` 那段）|
| edit | `src/traps.rs` | 把 `send_sig_stub` 换成真的 `send_sig` |
| edit | `src/sched/task.rs` | 补 `sigaction[32]`、`exit_signal`、亲子链的 `p_cptr`/`p_ysptr`/`p_osptr` |

### Closed decisions
- 沿用现有 `extern "C" start_kernel` ABI，不迁移到 `bootloader_api`（理由见 cerebrum Decision Log）
- 用 nightly，但**手写描述符结构体而不用 `x86_64` crate 的 `InterruptDescriptorTable`**：
  模块 4 实际实现时发现手写更贴合原版结构（原版 `_set_gate` 宏就是直接拼位），
  且能精确控制 IST 与 DPL。`x86_64` crate 仍在依赖里但目前未实际使用，
  下阶段若不再需要可以移除。
- 内核线程（`sched::kernel_thread`）保留：`sys_fork` 到位后它仍是跑
  bdflush/kswapd 那类纯内核任务的正确工具。
- 异常/中断里的打印统一走 `klib::printk` 的 `pr_*!` 宏，不要用 `kprintln!`

### Open decisions
- **`printk` 的临界区还没加**：`klib::printk::emit()` 的 SAFETY 注释假设
  「不与中断上下文并发」，但现在 `do_timer`/`do_IRQ`/`do_trap` 都会 printk。
  原版靠 `cli()`/`restore_flags()` 保护 `log_buf`。现在 `irq::local_irq_save`
  已经就绪，应该在下阶段开头就给 `emit()` 包上——这是已知的正确性缺口。
- **内核线程退出会泄漏一页内核栈**：`do_kthread_exit` 里没法释放自己
  正在用的栈。原版的做法是转 `TASK_ZOMBIE`，由 `release()` 在父进程
  `waitpid` 时回收。等 `exit.c` 移植完自然解决。
- **`sys_write` 缺 `verify_area`**：现在只接受落在恒等映射低 1GB 内的地址，
  用户态指针无法校验。要等 `mm/mmap.c` 的 `vm_area_struct` 才能做对。
- `syscall` 指令入口（`entry.S:syscall_entry`）留了符号但未启用，
  需要 per-cpu 的用户栈暂存位置。要不要在这一阶段启用待定。

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
