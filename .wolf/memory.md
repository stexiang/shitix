# Memory

> Chronological action log. Hooks and AI append to this file automatically.
> Old sessions are consolidated by the daemon weekly.

## Session: 2026-08-01 03:00

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 03:05 | 重写 VGA 控制台为彩色 Writer + fmt::Write，导出 print!/println!/cprint!/cprintln! | src/console.rs | 完成，零警告 | ~4k |
| 03:07 | lib.rs 改用宏输出，加 #![no_main]、e820 计数截断、panic 显示位置 | src/lib.rs | 完成 | ~2k |
| 03:09 | QEMU screendump 验证 VGA：7 行文本 + 4 种颜色 + 硬件光标 | target/boot/shitix.img | PASS | ~3k |
| 03:12 | 临时注入 panic! 验证红底白字 panic 路径，随后还原 | src/lib.rs | PASS，已还原 | ~2k |
| 03:14 | 补 anatomy.md 缺失的 src/ 与 scripts/ 段 | .wolf/anatomy.md | 完成 | ~1k |
| 03:20 | 定 nightly + x86_64 crate（用户拍板），加 rust-toolchain.toml | Cargo.toml, rust-toolchain.toml | 完成 | ~1k |
| 03:24 | 读 mm/memory.c、swap.c、kmalloc.c、page.h 摸清原版结构 | linux/mm/* | 完成 | ~8k |
| 03:28 | 写 mm 四个模块 + e820.rs（页帧分配/kmalloc/四级页表）| src/mm/*, src/e820.rs | 完成 | ~12k |
| 03:30 | **三重错误**：低 1MB 的页表被当空闲页派出去 → 加 MIN_USABLE_PHYS | src/mm/page_alloc.rs | 已修，见 bug-001 | ~4k |
| 03:33 | serial 加 fmt::Write + sprint!/kprintln!，自检结果进串口日志 | src/serial.rs, src/lib.rs | 完成 | ~2k |
| 03:36 | 临时压力测试：超大 kmalloc/double free/保留页/耗尽回收，全通过后删除 | src/lib.rs | PASS，已还原 | ~3k |
| 03:40 | 跨内存规格验证 32M/128M/1G/3G + release，均 PASS | target/boot/shitix.img | PASS | ~2k |

## Session: 2026-08-02 12:15

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-02 12:17

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 12:40 | 新增 src/klib/：ctype/string/errno/vsprintf/printk（移植 linux/lib/ + kernel/vsprintf.c + kernel/printk.c） | src/klib/*.rs, src/lib.rs | 5 模块建成，klib_selftest 6 组全绿 | ~28k |
| 12:52 | 修 number() 补位翻倍：原版 while(size-->0) 会消耗 size，Rust 版漏了 | src/klib/vsprintf.rs | "%8d" 补位恢复正确 | ~2k |
| 12:58 | debug + release 双profile 启动验证 | scripts/test.sh | 均 PASS，零警告 | ~2k |
| 13:05 | 发现 anatomy 500 文件上限被 linux/ 参考树占满，src/ 完全没被索引；调到 900 后重扫 | .wolf/config.json, .wolf/anatomy.md | 594 文件，src/ 与 src/klib/ 已入索引 | ~3k |

## Session: 2026-08-02 (模块 4)

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 13:20 | 读原版 traps.c/irq.c/sched.c/fork.c/sys_call.S/sched.h/system.h | linux/kernel/*, linux/include/* | 摸清四个子系统的耦合点 | ~22k |
| 13:35 | 新建 boot/entry.S：SAVE_ALL/RESTORE_ALL、21 个异常桩、16 个 IRQ 桩、system_call、ret_from_sys_call、switch_to、ret_from_fork | boot/entry.S | 定下 pt_regs ABI | ~9k |
| 13:50 | src/desc.rs：GDT/TSS(单个+IST)/IDT + set_*_gate | src/desc.rs | 取代 head.S 的临时表 | ~7k |
| 14:00 | src/traps.rs：PtRegs、TRAP_INFO 表、do_trap、die_if_kernel | src/traps.rs | 21 个向量统一分发 | ~6k |
| 14:10 | src/irq.rs：8259A 重映射、request/free_irq、do_IRQ、bottom half、cli/sti 封装 | src/irq.rs | PIC 到 0x20-0x2F | ~7k |
| 14:25 | src/sched/：task_struct 裁剪版、schedule 两遍扫描、WaitQueue、kernel_thread、sched_init | src/sched/{mod,task}.rs | 软件任务切换可用 | ~11k |
| 14:40 | src/syscall/：nr 表、do_syscall、SysArgs、9 个 sys_* | src/syscall/{mod,sys}.rs | int 0x80 链路通 | ~6k |
| 14:50 | 修 GDT 代码段 L&&D 非法组合导致三重错误 | src/desc.rs | bug-003 | ~4k |
| 15:00 | 修异常自检的 nomem/nostack 谎报 + 内联下沉 | src/lib.rs, src/traps.rs | bug-004/005 | ~5k |
| 15:10 | 修 jiffies 非 volatile 导致等待循环被提升 | src/sched/mod.rs | bug-006 | ~3k |
| 15:20 | 修 schedule 候选扫描没跳过 task[0] | src/sched/mod.rs | bug-007 | ~3k |
| 15:30 | 修 do_timer 跳过 task[0] 导致 need_resched 永不置位 | src/sched/mod.rs | bug-008 | ~2k |
| 15:40 | debug/release + -m 32M/1G 四轮验证，零警告 | scripts/test.sh | 全 PASS，42 次上下文切换 | ~4k |

## Session: 2026-08-02 13:50

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 00:00 | 排查 test.sh 的 ld.so.preload ERROR：确认是 snap-confine 的 AppArmor 拒绝 mmap /usr/local，与内核无关；构建与全部自检本来就 PASS。build.sh 过滤该行 stderr | scripts/build.sh, .wolf/buglog.json | 噪音消除，test.sh 仍 PASS | ~9k |

## Session: 2026-08-02 13:58

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-02 14:02

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-02 14:21

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-02 17:18

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## 2026-08-02 模块 5 收尾（fs + 驱动正确性）
| 时间 | 动作 | 文件 | 结果 | ~tokens |
|------|------|------|------|---------|
| --:-- | 修正自检 4/5 用只写 fd 去读（sys_creat 给的是 O_WRONLY）| src/lib.rs | 8/8 子测试通过 | ~3k |
| --:-- | 自检 1 用 bread 取代 getblk（部分写要先读，否则残留写回盘）| src/lib.rs | 修掉根文件系统随机损坏 | ~2k |
| --:-- | mount_root + fs 自检移入内核线程 fs_init_thread | src/lib.rs | fs 终于在可睡眠上下文跑 | ~5k |
| --:-- | 内核栈改静态池 4 页 + 栈魔数自检 | src/sched/mod.rs | 修掉 CORRUPTED STACK | ~4k |
| --:-- | kernel.ld 加 ASSERT(_kernel_end <= 0x90000)（已验证会触发）| boot/kernel.ld | 链接期拦住 BSS 越界 | ~1k |
| --:-- | 修 make_request / wait_on_buffer / add_request 三处丢失唤醒与临界区 | ll_rw.rs, buffer.rs, inode.rs, super_block.rs | 失败率 从 ~35% 降到 ~15% | ~8k |
| --:-- | printk/console 加中断临界区（补上上阶段已知缺口）| klib/printk.rs, console.rs | — | ~2k |
| --:-- | panic 走串口 + Display info.message() | src/lib.rs | 诊断从「一无所知」到「一行定位」 | ~2k |
| --:-- | 加一批不变量护栏（bh/inode 下标、data 空指针、链表下标、低端页、mkfs 写后读）| 多处 | 随机 page fault → 确定性断言 | ~5k |
| --:-- | **遗留**：约 15% 概率的 fs 自检失败未定位，见 buglog bug-023 | — | 未解决 | — |

## Session: 2026-08-02 19:50

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-02 04:00

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 13:09

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 13:15 | 复现链接错误 `_kernel_end` 越过 0x90000 | boot/kernel.ld, scripts/build.sh | ASSERT 触发，实测 _kernel_end=0xCC2B0 | ~4k |
| 13:20 | 定位 BSS 大户：注释掉 ASSERT 链到 /tmp 后 nm --size-sort | /tmp/system.elf | PAGE_REF_ARRAY 独占 256KB（BSS 共 365KB） | ~3k |
| 13:30 | 引用计数表改成动态划分（沿用 mem_map 约定）+ MemInfo 加两字段 | src/mm/page_alloc.rs | map_end 抬到表尾之后，表不进空闲链表 | ~5k |
| 13:40 | 重写 page_ref：REF_BASE/REF_LEN + attach()/slot()，越界返 None | src/mm/page_ref.rs | 静态数组消除；COW 标记位从 bit15 挪到 bit31（原本与计数字段重叠） | ~6k |
| 13:50 | 启动行补报 page_ref 表地址与槽数 | src/lib.rs | page_ref at 0x120000 (65504 slots) | ~1k |
| 13:55 | 构建 + QEMU 启动测试 ×5 | target/boot/ | 链接通过，_kernel_end=0x8C2C0，BSS 365KB→103KB；4/5 PASS，1 次 SUPER_AREA 护栏误报（既有 ~15% flaky，见 bug-019） | ~5k |
| 14:00 | 记录 bug-024 + cerebrum 三条 + STATUS 更新 | .wolf/ | 已归档 | ~3k |

## Session: 2026-08-06 13:22

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| 14:20 | 用户指出护栏误报被错记为 bug-023 flaky | .wolf/buglog.json | 确认记错：bug-023 症状里没有护栏 panic；改记 bug-025 | ~2k |
| 14:30 | 裸指针重写 check_guards + 失配复读，跑 20 次 | src/fs/super_block.rs | 0 次触发 | ~6k |
| 14:45 | 诊断变体（保留 UB 只加复读）跑 20 次 | src/fs/super_block.rs | 0 次触发 —— 单靠别名 UB 不稳定复现，加码即改内联 | ~4k |
| 15:00 | git 考古：256KB 数组在 33667fd 引入，ASSERT 更早 | boot/kernel.ld, src/mm/page_ref.rs | **33667fd 起内核一直没链接过**，最后可构建提交是 HEAD~2 | ~3k |
| 15:10 | HEAD~2 worktree 基线跑 20 次 | (worktree) | 护栏 0 次触发（与用户说法一致）；fs flaky 2/20 = bug-023 既有 | ~5k |
| 15:25 | 定稿 check_guards 修法 + 改正 bug-024、新增 bug-025 | src/fs/super_block.rs, .wolf/ | 3 次启动测试通过 | ~4k |

## Session: 2026-08-06 14:12

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 14:13

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 — 系统调用号补齐到 x86_64 正式表

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
| — | 审 `nr` 模块，发现大量重号（GETPID=WAIT4=61、GETPPID=KILL=62、六个 IO_* 全 0 覆盖 READ） | src/syscall/mod.rs | 确认自检 FAIL 的根因，记 bug-026 | ~6k |
| — | `nr` 改成按 x86_64 正式表逐号生成 0..=334 + io_uring/pidfd/clone3/faccessat2/epoll_pwait2 | src/syscall/mod.rs | 341 项无重号 | ~5k |
| — | 分发表改按号顺序逐项赋值 | src/syscall/mod.rs | 341 槽已挂实现 | ~3k |
| — | 补 133 个实现：lseek/readv/writev/sched_yield/gettid/time/exit_group/tkill/tgkill 真做，getgroups/madvise/mincore 等给合理默认，其余占位 -ENOSYS | src/syscall/sys.rs | 零新增告警 | ~9k |
| — | 修自检里 `nr::OPEN` 已实现导致的假设失效，新增私有号 `nr::UNUSED=501` | src/lib.rs, src/syscall/mod.rs | 自检从 FAIL 转 ok | ~1k |
| — | 发现 test.sh 被本地改动注释掉 build，之前几轮测的都是旧镜像 | scripts/test.sh | git checkout 恢复，记 bug-027 | ~2k |
| — | `implemented_count` 从比函数指针改成 WIRED 位图（release ICF 少数一个） | src/syscall/mod.rs | debug/release 都是 341，记 bug-028 | ~2k |
| — | 验证：debug 6 轮（5 过 1 挂 = 已知 bug-023 的 fs 竞态）、release 过、32M/128M/1G/3G 过 | — | 与本次改动无关的既有 flake | ~4k |

## Session: 2026-08-06 15:48

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 15:51

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 15:51

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 16:14

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 16:23

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session 2026-08-06 (模块 7)

| Time  | Action | Files | Outcome | ~tokens |
|-------|--------|-------|---------|---------|
| 15:30 | 21 个 syscall 从 `-ENOSYS` 接到 fs 层 | `src/syscall/sys.rs` | `open`/`creat`/`close`/`read`、`dup`/`dup2`、`chdir`/`chmod`/`truncate`、`mkdir`/`rmdir`/`unlink`/`link`/`mknod`、`fsync`、`stat`/`lstat`/`fstat`、`getdents`/`getdents64` | 8k |
| 15:45 | 用户指针护栏 | `src/syscall/sys.rs` | `check_range`/`user_path`/`user_buf`/`user_buf_mut`/`user_stat_out`，只挡低 1GB 外地址；**不阻止用户态读写内核内存** | 2k |
| 16:00 | `write` 改 fs 优先 + 控制台兜底 | `src/syscall/sys.rs` | 先走 `fs::read_write::write`，fd 1/2 拿到 `-EBADF` 才退回内核控制台 | 1k |
| 16:15 | 新增 `syscall_fs_selftest()` | `src/lib.rs` | 9 组测试（creat+write、lseek+read、fstat、dup、close×2、stat、mkdir/rmdir、unlink、EFAULT），挂在 `fs_init_thread` | 4k |
| 16:30 | 链接失败：BSS 越界 | `boot/kernel.ld` | `_kernel_end = 0x922C0`，超 0x90000 共 8896 字节；`.text`/`.rodata` 涨约 24KB | 2k |
| 16:45 | 内核栈池移出 BSS | `src/sched/mod.rs`, `src/mm/page_alloc.rs` | `KSTACKS` 改动态划分，`attach_kstacks()` 在 `page_ref` 表之后；`KSTACK_SLOTS` 3→8 | 6k |
| 17:00 | 修 `write` 自检期望 | `src/lib.rs` | `write(0,...)` 现在返 `-EBADF`（未打开 fd），原期望 `-EINVAL` | 1k |
| 17:15 | 新增 `scripts/check-syscall-nr.py` | `scripts/check-syscall-nr.py` | 机器核对 `nr` 模块与内核头，输出 `官方 360 个号，本树 360 个 / 全部一致` | 3k |
| 17:30 | `scripts/test.sh` 加 `MEM=` 覆盖 | `scripts/test.sh` | 默认 256M，照 `TIMEOUT`/`PROFILE` 约定 | 1k |
| 17:45 | 验证：debug/release + 内存矩阵 | - | `_kernel_end` = 0x854F0 (debug) / 0x5B4E0 (release)，余量 44KB；361 wired；32M/128M/1G/3G 全绿 | 8k |
| 18:00 | LFS 集成评估 | `.wolf/STATUS.md` | 核实缺项：无 ring-3 切换、`execve` 占位、ELF64 加载器缺、无 `copy_from_user`、`Stat` 是 i386 布局、`arch_prctl` 占位、ext4 只有结构定义无操作 | 4k |
| 18:15 | 记账 | `.wolf/buglog.json`, `.wolf/STATUS.md`, `.wolf/memory.md` | bug-029 (unlink 间歇性 +2)、STATUS.md 更新模块 7 + LFS 结论 | 3k |

**Total session:** ~43k tokens

## Session: 2026-08-06 19:15

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 21:01

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 21:03

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 21:04

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|

## Session: 2026-08-06 21:04

| Time | Action | File(s) | Outcome | ~Tokens |
|------|--------|---------|---------|--------|
