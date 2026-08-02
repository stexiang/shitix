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
