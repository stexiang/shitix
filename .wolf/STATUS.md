# STATUS — shitix

> Single source of truth for resuming work. Read this FIRST when starting a session.
> Update this file at the end of every work phase so the next `/clear` resumes in 1 read.
> Last updated: 2026-08-02 (模块 4 完成)

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

## 🚀 Next phase

**Goal:** 移植信号与进程生命周期 —— `kernel/signal.c` 的信号投递/`sigaction`、
`kernel/exit.c` 的 `do_exit`/`sys_waitpid` 收尸链、`kernel/fork.c` 的真正
`sys_fork`。这三者互相咬合，且是把现有调度器接到用户态的前提。

### Acceptance criteria
1. `send_sig(SIGSEGV, ...)` 能真正投递：用户态触发 page fault 后进程被杀而不是只打一行日志
2. `do_exit` 走完整流程：转 `TASK_ZOMBIE` → 通知父进程 → 父进程 `waitpid` 收尸并回收内核栈
3. `sys_fork` 能复制出一个真正的子进程（需要 `copy_page_tables`），父子各自返回不同的 pid
4. `ret_from_sys_call` 里接上 `do_signal`，返回用户态前投递待处理信号

### Files to create / edit
| Type | File | Content |
|---|---|---|
| new | `src/signal.rs` | `sigaction`/`sigset`/`send_sig`/`do_signal`（原版 `kernel/signal.c`）|
| new | `src/exit.rs` | `do_exit`/`sys_waitpid`/`notify_parent`/`release`（原版 `kernel/exit.c`）|
| new | `src/fork.rs` | `sys_fork`/`copy_process`（原版 `kernel/fork.c`）|
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
- **Key modules:** `src/lib.rs`(入口) / `src/console.rs`(VGA) / `src/serial.rs`(COM1) / `src/e820.rs` / `src/mm/`(page, page_alloc, kmalloc, paging) / `src/klib/`(ctype, string, errno, vsprintf, printk) / `src/desc.rs`(GDT/TSS/IDT) / `src/traps.rs` / `src/irq.rs` / `src/sched/`(task, mod) / `src/syscall/`(mod, sys) / `boot/*.S`(bootsect, setup, head, **entry**) + `boot/*.ld`
- **陷入/返回 ABI:** `boot/entry.S` 的 `SAVE_ALL` 压栈顺序 == `src/traps.rs` 的 `PtRegs` 字段顺序，改一侧必须同步另一侧。`orig_rax` 格三用途：系统调用号 / 异常错误码 / `!irq`
- **段选择子:** KERNEL_CS=0x08 KERNEL_DS=0x10 USER_CS=0x1B USER_DS=0x23 TSS=0x28（entry.S 里有同名 .set 常量，必须一致）
- **中断向量:** 0-20 异常（2/8/14 走 IST）、0x20-0x2F 是 PIC 重映射后的 IRQ0-15、0x80 是 int 0x80
- **系统调用约定:** 号在 rax，参数 rdi/rsi/rdx/r10/r8/r9（r10 而非 rcx，为兼容 syscall 指令）
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
