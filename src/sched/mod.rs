//! 进程调度。对应 linux-1.0.9 的 `kernel/sched.c` + `kernel/fork.c`。
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`task`] | `include/linux/sched.h` 的 `task_struct`/`tss_struct`/`INIT_TASK` |
//! | [`schedule`] | `sched.c:schedule()` |
//! | [`do_timer`] / [`JIFFIES`] | `sched.c:do_timer()` + 全局 `jiffies` |
//! | [`sleep_on`] / [`wake_up`] | `sched.c:__sleep_on()`/`wake_up()` |
//! | [`kernel_thread`] | 无（原版靠 fork+execve；见下） |
//! | [`init`] | `sched.c:sched_init()` |
//!
//! 三处与原版的结构性差异：
//!
//! 1. **软件任务切换**。原版 `switch_to(next)` 是一条 `ljmp` 到目标 TSS，
//!    CPU 全自动。long mode 删了这个机制，改为 `boot/entry.S:switch_to`
//!    手工保存 callee-saved + 换 `rsp`，并由 [`switch_to_task`] 顺带改写
//!    唯一那个 TSS 的 `rsp0`（原版每任务一个 TSS，不需要这步）。
//! 2. **task 用定长数组 + 下标而不是指针环**。原版 `task_struct` 里
//!    `next_task`/`prev_task` 是裸指针，构成一个环。Rust 里自引用结构要么
//!    上 `Pin` + `unsafe`，要么用下标；后者更清晰，且 `NR_TASKS` 本来就是定长。
//!    环的语义完整保留（[`Task::next`] 仍构成环，`schedule` 仍是遍历环）。
//! 3. **等待队列用侵入式的 task 下标链而不是栈上的 `wait_queue`**。原版
//!    `__sleep_on` 在调用者栈上放一个 `struct wait_queue wait = {current, NULL}`
//!    再挂进链表——这在 Rust 里是明目张膿的悬垂引用风险。我们改成每个
//!    等待队列存一个 task 下标链表头，链接字段放在 [`WaitQueue`] 自己的数组里。
//!
//! 尚未移植：`fork.c:sys_fork()` 的完整语义（要 `copy_page_tables`，依赖
//! `mm/mmap.c`）、信号投递（`kernel/signal.c`）、`exit.c` 的收尸逻辑。
//! 当前用 [`kernel_thread`] 造任务，够验证调度器本身。

pub mod task;

use crate::desc;
use crate::irq;
use crate::klib::errno::{EAGAIN, KResult};
use crate::klib::printk::Level;
use crate::mm;
use task::{STACK_MAGIC, flags};

// fs/ 与 drivers/ 需要这几个名字；原版它们都在 sched.h 里公开。
pub use task::{NR_TASKS, Task, TaskState, HZ};

/// syscall 指令的栈暂存区（定义在 entry.S .bss）。
mod syscall_scratch {
    unsafe extern "C" {
        pub static mut kernel_rsp_scratch: u64;
    }
}

/// 任务表。对应原版 `struct task_struct * task[NR_TASKS] = {&init_task, }`。
/// 原版是指针数组（槽位空 = NULL），我们是值数组（槽位空 = `TaskState::Unused`）。
static mut TASKS: [Task; NR_TASKS] = [const { Task::empty() }; NR_TASKS];

/// 当前任务下标。对应原版全局 `struct task_struct *current`。
static mut CURRENT: usize = 0;

/// 自启动以来的时钟滴答。对应原版 `unsigned long volatile jiffies`。
#[unsafe(no_mangle)]
pub static mut JIFFIES: u64 = 0;

/// 需要重新调度的标志。对应原版全局 `int need_resched`，
/// **`boot/entry.S` 的 `ret_from_sys_call` 直接引用这个符号**。
#[unsafe(no_mangle)]
pub static mut need_resched: i32 = 0;

/// 上下文切换计数。对应原版 `kstat.context_swtch`。
static mut CONTEXT_SWITCHES: u64 = 0;

/// 下一个要分配的 pid。对应原版 `last_pid`（在 `fork.c`）。
static mut LAST_PID: i32 = 0;

unsafe extern "C" {
    /// entry.S 的软件任务切换：保存 callee-saved 到当前栈，
    /// 把 rsp 写进 `*prev_rsp`，然后切到 `next_rsp`。
    fn switch_to(prev_rsp: *mut u64, next_rsp: u64);
    /// 内核线程的第一次入场点（entry.S）
    fn kernel_thread_entry();
}

// ---- 访问器 ----

/// 当前任务的下标。
#[inline]
pub fn current_nr() -> usize {
    // SAFETY: 只读一个 usize；单核下调度器改它时中断是关的。
    unsafe { *core::ptr::addr_of!(CURRENT) }
}

/// 当前任务。对应原版宏 `current`。
///
/// # Safety
/// 返回的引用在下一次 [`schedule`] 之前有效。调用方不得跨调度点持有。
pub unsafe fn current() -> &'static mut Task {
    // SAFETY: CURRENT 始终是有效槽位；契约要求不跨调度点持有。
    unsafe { &mut (*core::ptr::addr_of_mut!(TASKS))[*core::ptr::addr_of!(CURRENT)] }
}

/// 按下标取任务。
///
/// # Safety
/// 同 [`current`]，且 `nr < NR_TASKS`。
#[track_caller]
pub unsafe fn task(nr: usize) -> &'static mut Task {
    assert!(nr < NR_TASKS, "task(): index {} out of range (NR_TASKS={})", nr, NR_TASKS);
    // SAFETY: 上面已校验下标在界内。
    unsafe { &mut (*core::ptr::addr_of_mut!(TASKS))[nr] }
}

/// 任务的裸指针。
///
/// 调度环的链接字段（`next`/`prev`）必须用这个写，不能用 [`task`]。
/// [`task`] 返回 `&'static mut Task`；`task(nr).next = old_next` 与
/// `task(old_next).prev = nr` 在 `nr == old_next`（环上只剩一个任务）或
/// `cur == old_next` 时是对同一对象的两条可变引用。`&mut` 带 `noalias`，
/// LLVM 可以把后一次写丢掉，留下一个只连了一半的环节点。
/// `fs::buffer` 里的同一个错误造成了约 15% 概率的随机文件系统损坏
/// （见 `fs::buffer::buf_ptr` 的说明）。
///
/// # Safety
/// `nr < NR_TASKS`（内部断言）；调用者负责关中断或独占。
#[inline]
#[track_caller]
pub unsafe fn task_ptr(nr: usize) -> *mut Task {
    assert!(nr < NR_TASKS, "task_ptr(): index {} out of range", nr);
    // SAFETY: 下标已校验；TASKS 是地址恒定的静态数组。
    unsafe { (*core::ptr::addr_of_mut!(TASKS)).as_mut_ptr().add(nr) }
}

/// 当前任务的下标。等价于 `current_nr()`。
#[inline]
pub fn current_index() -> usize {
    current_nr()
}

/// 当前 jiffies。对应原版直接读全局 `jiffies`。
///
/// 必须用 `read_volatile`：原版把它声明成 `unsigned long **volatile** jiffies`
/// 正是为了这个——它只在时钟中断里自增，编译器看不到任何可见的写，
/// 于是会把 `while (jiffies() < deadline)` 里的读提到循环外，变成死循环。
#[inline]
pub fn jiffies() -> u64 {
    // SAFETY: 只读一个 u64；单核下 do_timer 的写是原子的。volatile 阻止提升。
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(JIFFIES)) }
}

/// 上下文切换总次数。
pub fn context_switches() -> u64 {
    // SAFETY: 只读一个 u64。
    unsafe { *core::ptr::addr_of!(CONTEXT_SWITCHES) }
}

/// 置 `need_resched`。对应原版 `need_resched = 1`。
#[inline]
pub fn set_need_resched() {
    // SAFETY: 单核下写一个 i32。
    unsafe { *core::ptr::addr_of_mut!(need_resched) = 1 }
}

// ---- 调度器主体 ----

/// 选一个任务跑。对应原版 `sched.c:schedule()`。
///
/// 沿用原版那个「两遍扫描」算法，一字不改地保留了它的语义：
/// 1. 第一遍处理睡眠超时和信号唤醒（原版还处理 itimer，我们没有 itimer）
/// 2. 第二遍找 `counter` 最大的 `TASK_RUNNING`
/// 3. 全部 `counter` 归零时，所有任务 `counter = counter/2 + priority`
///    —— 注意是**所有**任务而不只是可运行的，睡着的任务也在攒时间片，
///    这是原版有意的设计（醒来的任务优先级更高）
/// 4. 没有可运行任务就跑 task[0]（idle）
///
/// # Safety
/// 只能在「不在中断上下文」时调用（原版靠 `if (intr_count)` 检测并报
/// "Aiee: scheduling in interrupt"，我们同样检测）。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn schedule() {
    // 原版第一件事：中断里调度是严重错误
    // SAFETY: 只读一个 u64。
    if unsafe { *core::ptr::addr_of!(irq::intr_count) } != 0 {
        crate::pr!(Level::Err, "Aiee: scheduling in interrupt");
        // SAFETY: 原版也是直接清零硬着头皮继续。
        unsafe { *core::ptr::addr_of_mut!(irq::intr_count) = 0 }
    }

    // SAFETY: 下面整段独占任务表；进入时中断状态由调用方决定，
    // 我们在挑选任务期间关中断以免 do_timer 改 counter。
    let flags = unsafe { irq::local_irq_save() };

    // SAFETY: 单核，已关中断。
    unsafe { *core::ptr::addr_of_mut!(need_resched) = 0 }

    let now = jiffies();
    let cur = current_nr();

    // 第一遍：唤醒超时和收到信号的可打断睡眠者（原版 confuse_gcc1 前那段）
    // SAFETY: 遍历任务表，已关中断独占。
    unsafe {
        let tasks = &mut *core::ptr::addr_of_mut!(TASKS);
        for t in tasks.iter_mut() {
            if t.state != TaskState::Interruptible {
                continue;
            }
            if t.has_pending_signal() {
                t.state = TaskState::Running;
                continue;
            }
            if t.timeout != 0 && t.timeout <= now {
                t.timeout = 0;
                t.state = TaskState::Running;
            }
        }
    }

    // 第二遍：挑 counter 最大的 Running（原版 `c = -1` 到 confuse_gcc2 那段）。
    //
    // 关键：**扫描要跳过 task[0]**。原版的循环是
    //   next = p = &init_task;
    //   for (;;) { if ((p = p->next_task) == &init_task) goto confuse_gcc2; ... }
    // 先自增再判等，所以 init_task 自己从不作为候选参与比较，只作为
    // 「没有别的可运行任务」时的兜底默认值。
    // 我们的 task[0] 是 idle，counter 恒为 priority(15) 且 do_timer 不减它，
    // 一起参与比较的话它永远赢，别的任务一次都跑不上。见 buglog bug-006。
    let mut best = 0usize; // 兜底：task[0]，即原版的 init_task/idle
    let mut best_counter = -1i64;
    // SAFETY: 遍历任务环，已关中断独占。环由 kernel_thread/init 维护，
    // 保证从 0 出发沿 next 一定能回到 0。
    unsafe {
        let mut nr = task(0).next;
        while nr != 0 {
            let t = task(nr);
            if t.state == TaskState::Running && t.counter > best_counter {
                best_counter = t.counter;
                best = nr;
            }
            nr = t.next;
        }

        // 全部时间片耗尽 → 重新发放。原版：
        //   if (!c) for_each_task(p) p->counter = (p->counter >> 1) + p->priority;
        // 这里 `for_each_task` **包括** init_task（与上面的候选扫描不同）。
        // 注意也包括睡眠中的任务，这是原版的有意设计：睡着的任务也在攒
        // 时间片，醒来时优先级更高。
        if best_counter == 0 {
            let tasks = &mut *core::ptr::addr_of_mut!(TASKS);
            for t in tasks.iter_mut() {
                if t.state != TaskState::Unused {
                    t.counter = (t.counter >> 1) + t.priority;
                }
            }
            // 重新挑一次，否则这一轮还是会选到 counter=0 的那个
            best = 0;
            best_counter = -1;
            let mut nr = task(0).next;
            while nr != 0 {
                let t = task(nr);
                if t.state == TaskState::Running && t.counter > best_counter {
                    best_counter = t.counter;
                    best = nr;
                }
                nr = t.next;
            }
        }
    }

    if best == cur {
        // 无需切换。原版也是走到 switch_to 里由宏判断（`cmpl %ecx,_current; je 1f`）
        // SAFETY: 与上面的 save 配对。
        unsafe { irq::restore_flags(flags) };
        return;
    }

    // SAFETY: 只加一个计数器，已关中断。
    unsafe { *core::ptr::addr_of_mut!(CONTEXT_SWITCHES) += 1 }

    // SAFETY: best/cur 都是有效槽位；切换本身的前提见 switch_to_task 的契约。
    unsafe { switch_to_task(cur, best) };

    // 回到这里说明我们又被调度回来了。恢复调用方的中断状态。
    // SAFETY: 与上面的 save 配对（flags 在我们自己的内核栈上，切换后仍有效）。
    unsafe { irq::restore_flags(flags) };
}

/// 执行一次上下文切换。对应原版宏 `switch_to(next)`。
///
/// # Safety
/// 必须关中断调用；`prev`/`next` 必须是不同的有效槽位，且 `next` 的
/// `tss.rsp` 必须指向一个由 [`kernel_thread`] 或 [`init`] 正确布置过的内核栈。
unsafe fn switch_to_task(prev: usize, next: usize) {
    // SAFETY: 契约保证下标有效。
    let (prev_rsp_ptr, next_rsp, next_rsp0, next_cr3) = unsafe {
        let tasks = &mut *core::ptr::addr_of_mut!(TASKS);
        (
            core::ptr::addr_of_mut!(tasks[prev].tss.rsp),
            tasks[next].tss.rsp,
            tasks[next].tss.rsp0,
            tasks[next].pml4 as u64,
        )
    };

    // 先把 current 指向 next：switch_to 之后我们就在 next 的栈上了，
    // 那时读 CURRENT 必须已经是新值（原版由 `movl %edx,_current` 在
    // ljmp 之前完成，顺序相同）。
    // SAFETY: 已关中断，单核独占。
    unsafe { *core::ptr::addr_of_mut!(CURRENT) = next }

    // 改写唯一 TSS 的 rsp0，让下次从用户态陷入时落到 next 自己的内核栈。
    // 原版每任务一个 TSS，`ljmp` 时 CPU 自动加载，不需要这一步。
    if next_rsp0 != 0 {
        // SAFETY: next_rsp0 是 next 内核栈的栈顶，由创建时算好。
        unsafe { desc::set_rsp0(next_rsp0) }
        // 同步更新 syscall_entry 的内核栈 scratch
        unsafe { core::ptr::write_volatile(
            core::ptr::addr_of_mut!(syscall_scratch::kernel_rsp_scratch), next_rsp0) };
    }

    // 切页表。next_cr3==0 表示共用内核页表（启动 PML4=0x4000），
    // cur 可能是用户进程的 PML4，必须切回去——否则用户 PML4 被 release
    // free 掉之后内核侧 TLB 缺失会导致 #PF 读到已回收的页。
    // SAFETY: 读 cr3 合法；只在值不同时才写，避免无谓的 TLB 全刷。
    let kernel_cr3: u64 = 0x4000;
    let target_cr3 = if next_cr3 != 0 { next_cr3 } else { kernel_cr3 };
    unsafe {
        let cur_cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cur_cr3,
                         options(nomem, nostack, preserves_flags));
        if cur_cr3 != target_cr3 {
            // 换页表影响之后所有内存访问，不能声明 nostack。
            core::arch::asm!("mov cr3, {}", in(reg) target_cr3,
                             options(preserves_flags));
        }
    }

    // 切 FS/GS base（TLS）。只在值不同时才写 MSR。
    // SAFETY: CPL=0，wrmsr 合法。
    unsafe {
        let tasks = &*core::ptr::addr_of_mut!(TASKS);
        if tasks[prev].fs_base != tasks[next].fs_base {
            core::arch::asm!("wrmsr",
                in("ecx") 0xC000_0100u64,
                in("eax") tasks[next].fs_base as u32,
                in("edx") (tasks[next].fs_base >> 32) as u32,
                options(nomem, nostack, preserves_flags));
        }
        if tasks[prev].gs_base != tasks[next].gs_base {
            core::arch::asm!("wrmsr",
                in("ecx") 0xC000_0101u64,
                in("eax") tasks[next].gs_base as u32,
                in("edx") (tasks[next].gs_base >> 32) as u32,
                options(nomem, nostack, preserves_flags));
        }
    }

    // SAFETY: prev_rsp_ptr 指向 prev.tss.rsp；next_rsp 是 next 上次
    // switch_to 时保存的栈指针（或新任务的初始栈）。entry.S 的 switch_to
    // 会在那个栈上找到匹配的 callee-saved 保存区和返回地址。
    unsafe { switch_to(prev_rsp_ptr, next_rsp) }
}

/// 新任务第一次被调度到时的收尾。由 `entry.S:ret_from_fork` 和
/// `kernel_thread_entry` 调用。
///
/// 原版没有单独的函数（子进程直接从 `ret_from_sys_call` 起跑），
/// 但我们需要一个地方把「切换后」的状态补齐——原版靠 TSS 硬件切换
/// 自动完成的那部分。
///
/// # Safety
/// 只能由 entry.S 的两个新任务入场点调用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn schedule_tail() {
    // CLONE_CHILD_SETTID：子进程把自己的 pid 写到 set_child_tid 指向的
    // 用户地址（对应 Linux ret_from_fork 的 `put_user(tsk->pid, ...)`）。
    // 必须在子进程上下文做：写自己的地址空间，COW 缺页会正确复制出私有页，
    // 不会改穿父进程。在 clone 父进程上下文里写会腐败父进程的堆。
    //
    // 必须在 sti() 之前做：新任务被 switch_to 切进来时是关中断的，自己的栈
    // 上没有配对的 restore_flags。若先 sti 再写，定时器中断可能在写之前抢断、
    // 切到父进程；父进程 fork-parent atfork 会写共享 TLS 页，把子进程马上要
    // 读的自指针（fs:0x10）覆盖掉，导致子进程 robust-list 初始化读到 0、写穿
    // 第 0 页、级联腐败 malloc arena。先写完 SETTID 再开中断。
    // SAFETY: current 在子进程上下文里有效；set_child_tid 为 0 时跳过。
    unsafe {
        let cur = current();
        let tid = cur.pid;
        let addr = cur.set_child_tid;
        if addr != 0 {
            // 写用户地址：若该页是 COW 只读，supervisor 写会触发 #PF，
            // 由 traps.rs 的内核态 COW 处理路径复制后重试。
            core::ptr::write_volatile(addr as *mut i32, tid);
            // 一次性：写完即清，避免后续 fork 的子进程重复写老地址。
            cur.set_child_tid = 0;
        }
    }

    // 新任务是在关中断状态下被 switch_to 切进来的（schedule 里关的），
    // 但它自己的栈上没有配对的 restore_flags，所以在这里显式恢复。
    // SAFETY: IDT/PIC 已就绪；新任务预期运行在开中断状态。
    unsafe { irq::sti() }
}

// ---- 时钟中断（原版 do_timer）----

/// 时钟中断处理。对应原版 `sched.c:do_timer()`。
///
/// 原版这个函数前 60 行是 NTP 相锁环（`time_phase`/`time_adjust`/
/// `second_overflow`），依赖 `kernel/time.c` 的一整套时间校准状态机。
/// 那部分与调度无关，等移植 `time.c` 时再补；这里保留核心四件事：
/// jiffies 自增、时间片递减、utime/stime 统计、耗尽则置 need_resched。
fn do_timer(_irq: usize, regs: &mut crate::traps::PtRegs) {
    // SAFETY: 中断上下文，单核，jiffies 的自增不会与自身并发。
    unsafe { *core::ptr::addr_of_mut!(JIFFIES) += 1 }

    // 临时看门狗：每个滴答查一次超级块表有没有被写坏，坏了就把被打断的
    // RIP 打出来——那是「谁在写」的直接证据。只在 debug 构建里跑。
    if cfg!(debug_assertions) {
        // SAFETY: 只读一个 u16。
        if let Some(d) = unsafe { crate::fs::super_block::watchdog_bad_dev() } {
            crate::pr!(Level::Err,
                "WATCHDOG: SUPER_BLOCKS[0].s_dev={:#06x} interrupted rip={:#x} rsp={:#x} cs={:#x}",
                d, regs.rip, regs.rsp, regs.cs);
            panic!("SUPER_BLOCKS corrupted, interrupted rip={:#x}", regs.rip);
        }
        // inode 表两侧的护栏。破了说明是整块 memcpy 打偏（写穿了表的边界），
        // 完好而字段又是垃圾说明是坏下标/坏指针的定点写——两种成因的修法
        // 完全不同，所以先分开。
        // SAFETY: 只读。
        if let Some((side, k, v)) = unsafe { crate::fs::inode::watchdog_guards_ok() } {
            crate::pr!(Level::Err,
                "WATCHDOG: INODE_AREA guard_{}[{}]={:#x} interrupted rip={:#x} rsp={:#x} cs={:#x}",
                side, k, v, regs.rip, regs.rsp, regs.cs);
            panic!("INODE_AREA guard smashed, interrupted rip={:#x}", regs.rip);
        }
        // 同一套手法查 inode 表（bug-029）。见 watchdog_bad_inode 的文档。
        // SAFETY: 只读 inode 表。
        if let Some((n, m)) = unsafe { crate::fs::inode::watchdog_bad_inode() } {
            crate::pr!(Level::Err,
                "WATCHDOG: INODES[{}].i_mode={:#o} interrupted rip={:#x} rsp={:#x} cs={:#x}",
                n, m, regs.rip, regs.rsp, regs.cs);
            panic!("INODES corrupted at slot {}, interrupted rip={:#x}", n, regs.rip);
        }
    }

    // SAFETY: current 在中断里读是安全的（调度只发生在中断返回路径上）。
    let cur = unsafe { current() };

    // 原版：`if ((VM_MASK & regs->eflags) || (3 & regs->cs))` 判断被打断的
    // 是用户态还是内核态，据此计入 utime 或 stime。
    if regs.from_user() {
        cur.utime += 1;
    } else {
        cur.stime += 1;
    }

    // 间隔定时器（原版 sched.c:do_timer 里的 it_real 全表扫描 +
    // it_virt/it_prof 的当前任务递减；1.0.9 这段在 do_timer 开头）。
    // 到期发信号并用 incr 重装（incr=0 即一次性）。send_sig 只置位
    // 并唤醒，中断上下文里安全。
    unsafe {
        // ITIMER_REAL：墙钟，对所有任务走（不管它在不在跑）
        for i in 0..NR_TASKS {
            let t = task_ptr(i);
            if (*t).state == TaskState::Unused { continue; }
            if (*t).it_real_value != 0 {
                (*t).it_real_value -= 1;
                if (*t).it_real_value == 0 {
                    crate::signal::send_sig(crate::signal::Signal::SIGALRM as u32, i, 1);
                    (*t).it_real_value = (*t).it_real_incr;
                }
            }
        }
        // ITIMER_VIRTUAL：只算用户态 tick
        if regs.from_user() && cur.it_virt_value != 0 {
            cur.it_virt_value -= 1;
            if cur.it_virt_value == 0 {
                crate::signal::send_sig(crate::signal::Signal::SIGVTALRM as u32, current_index(), 1);
                cur.it_virt_value = cur.it_virt_incr;
            }
        }
        // ITIMER_PROF：用户态+内核态都算
        if cur.it_prof_value != 0 {
            cur.it_prof_value -= 1;
            if cur.it_prof_value == 0 {
                crate::signal::send_sig(crate::signal::Signal::SIGPROF as u32, current_index(), 1);
                cur.it_prof_value = cur.it_prof_incr;
            }
        }
    }

    // 时间片递减。原版这段在 do_timer 末尾：
    //   if ((--current->counter)<=0) { current->counter = 0; need_resched = 1; }
    //
    // **对 task[0] 也要做**。曾经在这里加了 `if current_nr() != 0` 的守卫，
    // 想着「idle 不该参与时间片核算」——结果 idle 的 counter 永不归零，
    // need_resched 永不置位，而 idle 循环正是靠轮询它才会让出 CPU，
    // 于是新建的任务一次都跑不上。见 buglog bug-007。
    cur.counter -= 1;
    if cur.counter <= 0 {
        cur.counter = 0;
        set_need_resched();
    }
}

// ---- 睡眠与唤醒（原版 __sleep_on / wake_up）----

/// 等待队列。对应原版 `struct wait_queue`，但结构完全不同——
/// 见模块文档第 3 点：原版把节点放在调用者栈上，我们改成侵入式下标链。
pub struct WaitQueue {
    /// 队首任务的下标，`NR_TASKS` 表示空队列
    head: usize,
}

/// 每个任务在等待队列里的「下一个」指针。原版是 `wait_queue.next`，
/// 放在栈上的节点里；我们按任务下标存，避免悬垂引用。
static mut WAIT_NEXT: [usize; NR_TASKS] = [NR_TASKS; NR_TASKS];

impl WaitQueue {
    /// 一个空队列。对应原版 `struct wait_queue * q = NULL`。
    pub const fn new() -> Self {
        WaitQueue { head: NR_TASKS }
    }

    /// 队列是否为空。
    pub fn is_empty(&self) -> bool {
        // SAFETY: 读共享状态用 volatile，防止编译器把 head 缓存进寄存器，
        // 与睡眠侧通过裸指针的写失去同步（丢失唤醒）。
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(self.head)) >= NR_TASKS }
    }

    /// 把当前任务挂进队列并睡下，直到被 [`wake_up`](Self::wake_up) 唤醒。
    /// 对应原版 `sleep_on()`（不可打断）。
    ///
    /// # Safety
    /// 不能在中断上下文或 task[0] 里调用——原版对此有明确检查
    /// （`if (current == task[0]) panic("task[0] trying to sleep")`）。
    pub unsafe fn sleep_on(&mut self) {
        // SAFETY: 契约转交。
        unsafe { self.sleep_on_state(TaskState::Uninterruptible, 0) }
    }

    /// 可被信号打断的睡眠。对应原版 `interruptible_sleep_on()`。
    ///
    /// # Safety
    /// 同 [`sleep_on`](Self::sleep_on)。
    pub unsafe fn interruptible_sleep_on(&mut self) {
        // SAFETY: 契约转交。
        unsafe { self.sleep_on_state(TaskState::Interruptible, 0) }
    }

    /// 带超时的可打断睡眠。原版没有这个组合（1.0.9 靠调用方自己设
    /// `current->timeout` 再 `interruptible_sleep_on`），这里包成一个函数。
    ///
    /// # Safety
    /// 同 [`sleep_on`](Self::sleep_on)。
    pub unsafe fn sleep_on_timeout(&mut self, ticks: u64) {
        // SAFETY: 契约转交。
        unsafe { self.sleep_on_state(TaskState::Interruptible, jiffies() + ticks) }
    }

    /// 「先挂队列、再在关中断下复查条件」的睡眠。对应原版
    /// `__wait_on_buffer` 的结构：
    /// ```c
    /// bh->b_count++;
    /// add_wait_queue(&bh->b_wait, &wait);
    /// repeat:
    ///     current->state = TASK_UNINTERRUPTIBLE;
    ///     if (bh->b_lock) { schedule(); goto repeat; }
    ///     remove_wait_queue(...); bh->b_count--;
    /// ```
    /// 关键在于**先置 state 再判条件**：反过来（先判条件、条件成立才去
    /// 睡）的话，判完到真正挂上队列之间有一个窗口，中断里的 `wake_up`
    /// 正好落在这个窗口就没人收到，任务永久睡死。在缓冲上表现为某个
    /// `b_count` 再也回不到 1，`minix_truncate` 于是一直 retry 到放弃、
    /// 漏掉那个块。
    ///
    /// `cond` 在关中断状态下被反复求值，必须短且不睡。
    ///
    /// # Safety
    /// 同 [`sleep_on`](Self::sleep_on)。`cond` 不得再次进入调度器。
    pub unsafe fn sleep_on_while(&mut self, mut cond: impl FnMut() -> bool) {
        let nr = current_nr();
        if nr == 0 {
            panic!("task[0] trying to sleep");
        }
        // SAFETY: 全程关中断，只在 schedule() 内部放开。
        let flags = unsafe { irq::local_irq_save() };
        // 先挂上队列（原版 add_wait_queue 在 repeat 之前）
        // SAFETY: 已关中断，独占等待链。
        unsafe {
            (*core::ptr::addr_of_mut!(WAIT_NEXT))[nr] =
                core::ptr::read_volatile(core::ptr::addr_of!(self.head));
        }
        core::ptr::write_volatile(core::ptr::addr_of_mut!(self.head), nr);
        while cond() {
            // SAFETY: 已关中断，独占任务表。
            unsafe {
                let t = task(nr);
                t.state = TaskState::Uninterruptible;
                t.timeout = 0;
            }
            // SAFETY: 不在中断上下文（契约保证）；schedule 内部会开中断。
            unsafe { schedule() };
        }
        // SAFETY: 已关中断。
        unsafe { task(nr).state = TaskState::Running };
        self.remove(nr);
        // SAFETY: 与上面的 save 配对。
        unsafe { irq::restore_flags(flags) };
    }

    /// # Safety
    /// 见 [`sleep_on`](Self::sleep_on)。
    unsafe fn sleep_on_state(&mut self, state: TaskState, timeout: u64) {
        let nr = current_nr();
        if nr == 0 {
            // 原版：panic("task[0] trying to sleep")
            panic!("task[0] trying to sleep");
        }
        // SAFETY: 挂链期间关中断，防止 wake_up 从中断里插进来看到半截链表。
        let flags = unsafe { irq::local_irq_save() };
        // SAFETY: nr 有效；独占任务表与等待链。
        unsafe {
            let t = task(nr);
            t.state = state;
            t.timeout = timeout;
            (*core::ptr::addr_of_mut!(WAIT_NEXT))[nr] =
                core::ptr::read_volatile(core::ptr::addr_of!(self.head));
        }
        core::ptr::write_volatile(core::ptr::addr_of_mut!(self.head), nr);

        // 原版 __sleep_on 在 schedule() 之前 sti()：睡下之后必须能被中断唤醒。
        // SAFETY: IDT/PIC 就绪。
        unsafe { irq::sti() };
        // SAFETY: 不在中断上下文（契约保证）。
        unsafe { schedule() };

        // 醒了。把自己从队列里摘掉（原版 remove_wait_queue）。
        self.remove(nr);
        // SAFETY: 与上面的 save 配对。
        unsafe { irq::restore_flags(flags) };
    }

    /// 把某个任务从队列里摘除。对应原版 `remove_wait_queue()`。
    fn remove(&mut self, nr: usize) {
        // SAFETY: 摘链期间关中断。
        let flags = unsafe { irq::local_irq_save() };
        // SAFETY: 独占等待链。
        unsafe {
            let next = &mut *core::ptr::addr_of_mut!(WAIT_NEXT);
            let head = core::ptr::read_volatile(core::ptr::addr_of!(self.head));
            if head == nr {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(self.head), next[nr]);
            } else {
                let mut p = head;
                while p < NR_TASKS {
                    if next[p] == nr {
                        next[p] = next[nr];
                        break;
                    }
                    p = next[p];
                }
            }
            next[nr] = NR_TASKS;
        }
        // SAFETY: 与上面的 save 配对。
        unsafe { irq::restore_flags(flags) };
    }

    /// 唤醒队列上所有任务。对应原版 `wake_up()`。
    ///
    /// 原版注释特别说明这里**不需要** cli/sti 配对，因为中断只会调
    /// `wake_up` 而不会直接改队列结构。我们的链表结构不同（侵入式），
    /// 中断里唤醒和进程里挂链会碰同一个 `WAIT_NEXT`，所以还是要关中断。
    pub fn wake_up(&mut self) {
        self.wake_up_state(false);
    }

    /// 只唤醒可打断睡眠的任务。对应原版 `wake_up_interruptible()`。
    pub fn wake_up_interruptible(&mut self) {
        self.wake_up_state(true);
    }

    fn wake_up_state(&mut self, only_interruptible: bool) {
        // SAFETY: 遍历等待链期间关中断。
        let flags = unsafe { irq::local_irq_save() };
        // SAFETY: 独占任务表与等待链。
        unsafe {
            let cur_counter = task(current_nr()).counter;
            let next = &*core::ptr::addr_of!(WAIT_NEXT);
            let mut p = core::ptr::read_volatile(core::ptr::addr_of!(self.head));
            while p < NR_TASKS {
                let t = task(p);
                let wakeable = match t.state {
                    TaskState::Interruptible => true,
                    TaskState::Uninterruptible => !only_interruptible,
                    _ => false,
                };
                if wakeable {
                    t.state = TaskState::Running;
                    t.timeout = 0;
                    // 原版：被唤醒的任务时间片更多就立刻要求重新调度
                    if t.counter > cur_counter {
                        set_need_resched();
                    }
                }
                p = next[p];
            }
        }
        // SAFETY: 与上面的 save 配对。
        unsafe { irq::restore_flags(flags) };
    }
}

/// 主动让出 CPU。对应原版 `sys_pause()` 的调度部分 / `schedule()` 的直接调用。
///
/// # Safety
/// 不能在中断上下文调用。
pub unsafe fn yield_now() {
    set_need_resched();
    // SAFETY: 契约保证不在中断里。
    unsafe { schedule() }
}

/// 睡指定的滴答数。原版的等价物是 `sys_pause` + `current->timeout`
/// 或 `add_timer`；这里做成最简形式供自检用。
///
/// # Safety
/// 不能在中断上下文或 task[0] 里调用。
pub unsafe fn sleep_ticks(ticks: u64) {
    let nr = current_nr();
    if nr == 0 {
        panic!("task[0] trying to sleep");
    }
    // SAFETY: 契约保证调用环境；只改自己的状态然后让出。
    unsafe {
        let t = task(nr);
        t.state = TaskState::Interruptible;
        t.timeout = jiffies() + ticks;
        irq::sti();
        schedule();
    }
}

// ---- 创建任务（原版 fork.c）----

/// 找一个空槽位并分配 pid。对应原版 `fork.c:find_empty_process()`。
///
/// # Safety
/// 必须关中断调用（原版靠 `sys_fork` 的调用环境保证）。
unsafe fn find_empty_process() -> KResult<usize> {
    // SAFETY: 契约保证已关中断，独占任务表与 LAST_PID。
    unsafe {
        let pid_p = core::ptr::addr_of_mut!(LAST_PID);
        *pid_p += 1;
        let tasks = &*core::ptr::addr_of!(TASKS);
        // 槽位 0 永远是 idle，从 1 开始找
        for nr in 1..NR_TASKS {
            if tasks[nr].state == TaskState::Unused {
                return Ok(nr);
            }
        }
    }
    // 原版 sys_fork 在这里返回 -EAGAIN
    Err(EAGAIN)
}

/// 创建一个内核线程。
///
/// 原版**没有**这个函数：1.0.9 的 `init` 是 `sys_fork()` 出来后
/// `execve("/bin/sh")` 的用户态进程，内核里没有「只跑内核代码的任务」这个概念
/// （`task[0]` 是唯一例外，而它是静态构造的 `INIT_TASK`）。
///
/// 我们需要它是因为 `sys_fork` 的完整语义要 `copy_page_tables`（依赖
/// `mm/mmap.c` 的 `vm_area_struct`，尚未移植），而调度器本身现在就该验证。
/// 内核线程共用内核页表，绕开了整个 VM 复制问题。
///
/// 栈布局（从高到低，与 `entry.S:switch_to` 的保存顺序严格互补）：
/// ```text
///   栈顶 - 8   : arg           \ kernel_thread_entry 用 pop 取
///   栈顶 - 16  : fn            /
///   栈顶 - 24  : kernel_thread_entry   ← switch_to 的 ret 跳到这里
///   栈顶 - 32  : rflags(IF=0)  \
///   栈顶 - 40  : r15            |
///   ...                         | switch_to 的 popfq/pop 序列按此顺序取
///   栈顶 - 80  : rbp           /
///                             ← tss.rsp 指这里
/// ```
/// 每个内核线程的栈页数。见 [`kernel_thread`] 里关于「一页不够」的说明。
pub const KSTACK_PAGES: usize = 4;
/// 池里放几份栈。task[0] 不占一份（它用 head.S 里的静态栈）。
///
/// 曾经是 3，因为那时池子放在 BSS 里，而 BSS 一旦长过 0x90000 就会盖掉
/// setup.S 留在那里的机器参数与 E820 表（0x90000 参数区、0x9E000 E820
/// 数组），表现为 mm 初始化前就 panic；链接脚本的
/// `ASSERT(_kernel_end <= 0x90000)` 拦住过这类事故四次。现在池子由
/// `page_alloc::init` 从物理内存里划出（见 [`attach_kstacks`]），BSS 不再
/// 随槽位数增长，所以可以放宽到 8 份。
pub const KSTACK_SLOTS: usize = 8;
/// 内核栈字节数。
pub const KSTACK_SIZE: usize = KSTACK_PAGES * mm::PAGE_SIZE;
/// 整个池的字节数。`page_alloc::init` 按这个数划地。
pub const KSTACK_POOL_BYTES: usize = KSTACK_SIZE * KSTACK_SLOTS;

/// 内核栈池的底地址，由 [`attach_kstacks`] 在 `page_alloc::init` 里填好。
/// 0 表示还没挂上（此时 [`alloc_kstack`] 一律失败）。
///
/// 池首地址页对齐，所以每份栈的底地址也都是页对齐的，和原版
/// `kernel_stack_page` 的性质一致（`current` 曾经靠屏蔽低位从 rsp 反推
/// task，我们不用那个技巧，但保持对齐没有坏处）。
static mut KSTACK_BASE: usize = 0;
/// 哪一份已被占用。这个是定长 bool 数组，留在 BSS 里无所谓（8 字节）。
static mut KSTACK_USED: [bool; KSTACK_SLOTS] = [false; KSTACK_SLOTS];

/// 把 `page_alloc::init` 划出的那块地登记为内核栈池。
///
/// # Safety
/// 只能由 `page_alloc::init` 调用一次，且必须在任何 [`kernel_thread`]
/// 之前。`base` 必须页对齐，且 `base..base + KSTACK_POOL_BYTES` 是恒等
/// 映射、已清零、不会再被派发给别人的物理内存。
pub unsafe fn attach_kstacks(base: usize) {
    // SAFETY: 契约保证此时是启动早期的独占阶段，无并发访问。
    unsafe { *core::ptr::addr_of_mut!(KSTACK_BASE) = base };
}

/// 上一个退出的内核线程留下的栈，等下一次分配时回收。见
/// [`do_kthread_exit`] 里的说明。0 表示没有待回收的。
static mut KSTACK_PENDING: usize = 0;

/// 回收上一个退出的线程留下的栈。
///
/// # Safety
/// 同 [`alloc_kstack`]。
unsafe fn reap_kstack() {
    // SAFETY: 契约转交。
    unsafe {
        let p = *core::ptr::addr_of!(KSTACK_PENDING);
        if p != 0 {
            *core::ptr::addr_of_mut!(KSTACK_PENDING) = 0;
            free_kstack(p);
        }
    }
}

/// 取一份内核栈，返回栈底地址，0 表示池已满。
///
/// # Safety
/// 只能在关中断的情况下调用（[`kernel_thread`] 已经关了）。
pub unsafe fn alloc_kstack() -> usize {
    // SAFETY: 契约保证已关中断、单核独占这两个静态量。
    unsafe {
        reap_kstack();
        let base = *core::ptr::addr_of!(KSTACK_BASE);
        // 池还没挂上（mm 未初始化）——不可能发生，但宁可返回失败也别派 0 页。
        if base == 0 {
            return 0;
        }
        let used = &mut *core::ptr::addr_of_mut!(KSTACK_USED);
        for (i, u) in used.iter_mut().enumerate() {
            if !*u {
                *u = true;
                return base + i * KSTACK_SIZE;
            }
        }
        0
    }
}

/// 归还一份内核栈。
///
/// # Safety
/// 同 [`alloc_kstack`]；`addr` 必须是它返回过的地址。
pub unsafe fn free_kstack(addr: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let base = *core::ptr::addr_of!(KSTACK_BASE);
        if base == 0 || addr < base {
            return;
        }
        let i = (addr - base) / KSTACK_SIZE;
        let used = &mut *core::ptr::addr_of_mut!(KSTACK_USED);
        if i < used.len() {
            used[i] = false;
        }
    }
}

pub fn kernel_thread(name: &str, entry: fn(u64), arg: u64, priority: i64) -> KResult<usize> {
    // SAFETY: 全程关中断，独占任务表。
    let flags = unsafe { irq::local_irq_save() };
    let result = (|| -> KResult<usize> {
        // SAFETY: 已关中断。
        let nr = unsafe { find_empty_process()? };

        // 内核栈：从内核栈池里取 [`KSTACK_PAGES`] 页连续空间。
        //
        // 原版 `sys_fork` 用 `get_free_page()` 取**一**页（4KB）当
        // `kernel_stack_page`，我们最初也照搬了，但实测不够：原版的 i386
        // 栈帧比 x86_64 小（寄存器少一半、指针 4 字节 vs 8 字节），而
        // fs 的调用链很深（`sys_open` → `namei` → `dir_namei` →
        // `minix_lookup` → `find_entry` → `minix_bread` → `getblk` →
        // `ll_rw_block` → `do_rd_request`）。一页会溢出，症状是栈底魔数
        // 被踩掉 + 一个 CR2 是小负数的 page fault（见 buglog）。
        //
        // 页分配器只能给单页且不保证相邻，所以这里用预划的池而不是
        // `get_free_page`：连续、对齐、无需回收，代价是固定占用。池本身由
        // `page_alloc::init` 从物理内存里划出（见 [`attach_kstacks`]），不占 BSS。
        let stack = unsafe { alloc_kstack() };
        if stack == 0 {
            return Err(EAGAIN);
        }

        // 栈底放魔数，对应原版 `*(unsigned long *)p->kernel_stack_page = STACK_MAGIC`
        // SAFETY: stack 是刚分配、我们独占的整页。
        unsafe { core::ptr::write_volatile(stack as *mut u64, STACK_MAGIC) };

        let stack = stack as u64;
        let stack_top = stack + KSTACK_SIZE as u64;

        // 布置初始栈。SAFETY: stack_top 往下 80 字节都在我们刚分配的页内
        // （页是 4096 字节，远大于 80），且该页无人共享。
        let rsp = unsafe {
            let mut sp = stack_top as *mut u64;
            // kernel_thread_entry 用 pop 依次取 fn 和 arg，所以 arg 在高位
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, arg);
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, entry as *const () as u64);
            // switch_to 的 `ret` 会跳到这个地址
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, kernel_thread_entry as *const () as u64);
            // switch_to 的 popfq 取这个。IF=0：新任务由 schedule_tail 里的
            // sti 打开中断，与 schedule() 关中断进入的语义一致。
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, 0x0002); // 保留位 1 恒为 1
            // switch_to 的 pop r15/r14/r13/r12/rbx/rbp 六个
            for _ in 0..6 {
                sp = sp.sub(1);
                core::ptr::write_volatile(sp, 0);
            }
            sp as u64
        };

        // SAFETY: nr 是刚找到的空槽位，已关中断独占。
        unsafe {
            let t = task(nr);
            *t = Task::empty();
            t.set_name(name);
            t.state = TaskState::Running;
            t.priority = priority;
            t.counter = priority;
            t.flags = flags::PF_KTHREAD;
            t.pid = *core::ptr::addr_of!(LAST_PID);
            t.parent = current_nr();
            t.start_time = jiffies();
            t.kernel_stack = stack;
            t.tss.rsp = rsp;
            t.tss.rsp0 = stack_top;
            // 共用内核页表（pml4=0；原版 fork 会 copy_page_tables 出一份新的）
            t.pml4 = 0;

            // 挂进调度环。原版 `SET_LINKS(p)` 操作 next_task/prev_task 指针，
            // 我们操作下标，语义相同。
            // 裸指针写：nr/cur/old_next 可能两两相等，见 [`task_ptr`]。
            let cur = current_nr();
            let old_next = (*task_ptr(cur)).next;
            (*task_ptr(nr)).next = old_next;
            (*task_ptr(nr)).prev = cur;
            (*task_ptr(cur)).next = nr;
            (*task_ptr(old_next)).prev = nr;
        }
        Ok(nr)
    })();
    // SAFETY: 与上面的 save 配对。
    unsafe { irq::restore_flags(flags) };
    result
}

/// 内核线程返回时的收尾。由 `entry.S:kernel_thread_entry` 调用。
/// 原版没有对应物（内核线程概念本身就是新增的）。
///
/// # Safety
/// 只能由 entry.S 在内核线程的入口函数返回后调用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn do_kthread_exit(code: u64) {
    let nr = current_nr();
    // SAFETY: 契约保证我们在某个内核线程的栈上。
    unsafe {
        let flags = irq::local_irq_save();
        let t = task(nr);
        t.exit_code = code as i32;
        // 原版 do_exit 会转成 TASK_ZOMBIE 等父进程 wait；我们没有 wait，
        // 直接释放槽位和栈。
        let stack = t.kernel_stack;
        // 摘出调度环
        // 裸指针写：p/n/自己可能相等，见 [`task_ptr`]。
        let (p, n) = (t.prev, t.next);
        (*task_ptr(p)).next = n;
        (*task_ptr(n)).prev = p;
        t.state = TaskState::Unused;
        t.kernel_stack = 0;
        irq::restore_flags(flags);

        // 此刻我们还在这个栈上跑，不能立刻归还——归还后它可能被
        // 下一个 kernel_thread 拿去初始化，而我们还要在上面跑到
        // schedule() 切走为止。所以只记下来，由下一次 alloc_kstack
        // 之前的 reap_kstack 回收。
        //
        // 原版的做法是转 TASK_ZOMBIE、由父进程 waitpid 时 release()
        // 回收；等 exit.c 移植完再换成那个。
        *core::ptr::addr_of_mut!(KSTACK_PENDING) = stack as usize;

        set_need_resched();
        schedule();
    }
    // schedule 不会切回一个 Unused 的任务，所以到不了这里
    unreachable!("dead kernel thread was scheduled");
}

// ---- 初始化（原版 sched_init）----

/// 8253/8254 可编程间隔定时器的端口与分频值。
/// 对应原版 `sched_init()` 末尾那三行 outb 和 `include/linux/timex.h` 的 `LATCH`。
const PIT_CH0: u16 = 0x40;
const PIT_CMD: u16 = 0x43;
/// 1193180 Hz / HZ。对应原版 `#define LATCH (1193180/HZ)`。
const LATCH: u16 = (1_193_180 / HZ) as u16;

/// 初始化调度器。对应原版 `kernel/sched.c:sched_init()`。
///
/// # Safety
/// 必须在 `desc::init_gdt`/`init_idt`、`irq::init`、`mm::init` 之后，
/// 中断关闭时调用一次。
pub unsafe fn init() -> KResult<()> {
    // task[0] = idle。对应原版静态构造的 `INIT_TASK`（comm 是 "swapper"）。
    // 它用的是 head.S 的静态内核栈，所以 kernel_stack = 0（无魔数可查）。
    // SAFETY: 启动期单线程，独占任务表。
    unsafe {
        let t = task(0);
        *t = Task::empty();
        t.set_name("swapper");
        t.state = TaskState::Running;
        t.priority = 15;
        t.counter = 15;
        t.flags = flags::PF_KTHREAD;
        t.pid = 0;
        t.parent = 0;
        // 单元素环：next/prev 都指向自己（原版 INIT_TASK 里
        // `/* schedlink */ &init_task,&init_task`）
        t.next = 0;
        t.prev = 0;
        *core::ptr::addr_of_mut!(CURRENT) = 0;
        *core::ptr::addr_of_mut!(LAST_PID) = 0;
    }

    // 编程 8253：binary, mode 2 (rate generator), LSB/MSB, channel 0。
    // 逐字对应原版 `outb_p(0x34,0x43); outb_p(LATCH & 0xff,0x40); outb(LATCH >> 8,0x40)`。
    // SAFETY: CPL=0；这是 PIT 的标准初始化序列。
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") PIT_CMD, in("al") 0x34u8,
            options(nomem, nostack, preserves_flags)
        );
        core::arch::asm!(
            "out dx, al",
            in("dx") PIT_CH0, in("al") (LATCH & 0xFF) as u8,
            options(nomem, nostack, preserves_flags)
        );
        core::arch::asm!(
            "out dx, al",
            in("dx") PIT_CH0, in("al") (LATCH >> 8) as u8,
            options(nomem, nostack, preserves_flags)
        );
    }

    // 注册 timer 中断。对应原版
    // `if (request_irq(TIMER_IRQ,(void (*)(int)) do_timer)!=0) panic(...)`。
    // fast=true（原版的 do_timer 走的是普通 do_IRQ 路径，但那会在时钟处理里
    // 开中断从而允许时钟自身重入；我们的 do_timer 改 counter，重入会算错，
    // 所以走 SA_INTERRUPT 语义）。
    irq::request_irq(0, do_timer, true)?;

    Ok(())
}

/// 打印当前任务信息。`traps::die_if_kernel` 调它补全「Process xxx (pid: n)」那行。
pub fn print_current() {
    let nr = current_nr();
    // SAFETY: CURRENT 始终有效。
    let t = unsafe { task(nr) };
    crate::pr!(Level::Emerg,
               "Process {} (pid: {}, task nr: {}, stack: {:#x}{})",
               t.name(), t.pid, nr, t.kernel_stack,
               if t.stack_ok() { "" } else { ", CORRUPTED STACK" });
}

// ---- task[0] 的静态栈护栏（原版没有）----

unsafe extern "C" {
    /// `head.S` 里栈底之下那一页哨兵。
    #[link_name = "stack_guard"]
    static STACK_GUARD: u8;
    /// `head.S` 的内核栈低端。
    #[link_name = "stack_bottom"]
    static STACK_BOTTOM: u8;
    /// 内核栈高端（初始 rsp）。
    #[link_name = "stack_top"]
    static STACK_TOP: u8;
}

/// 哨兵页的填充值。选一个不会被误撞出来的模式。
const GUARD_PAT: u64 = 0x5A5A_C0DE_5A5A_C0DE;

/// 哨兵区大小，必须与 `head.S` 里 `stack_guard` 的 `.space` 一致。
const GUARD_BYTES: usize = 512;

/// 给 task[0] 的静态栈铺哨兵。必须在 `start_kernel` 早期、任何深调用
/// 之前调用一次。
///
/// # Safety
/// 启动早期调用一次，此时哨兵页无人使用（它只是 `.bss` 里的填充）。
pub unsafe fn init_stack_guard() {
    // SAFETY: stack_guard 是链接器给出的 .bss 内 4096 字节区域。
    unsafe {
        let p = core::ptr::addr_of!(STACK_GUARD) as *mut u64;
        for i in 0..(GUARD_BYTES / 8) {
            core::ptr::write_volatile(p.add(i), GUARD_PAT);
        }
    }
}

/// 哨兵是否完好。false 表示 task[0] 的栈已经溢出，踩到了 `.bss` 里
/// 排在它之前的东西（`fs::buffer::BUFFERS` 就在那一片）。
pub fn stack_guard_ok() -> bool {
    // SAFETY: 只读哨兵页。
    unsafe {
        let p = core::ptr::addr_of!(STACK_GUARD) as *const u64;
        for i in 0..(GUARD_BYTES / 8) {
            if core::ptr::read_volatile(p.add(i)) != GUARD_PAT {
                return false;
            }
        }
        true
    }
}

/// task[0] 静态栈的用量高水位（字节）。靠「从栈底往上找第一个非零字」
/// 估算——`head.S` 已经把整个 `.bss` 清过零，所以还没被碰过的部分是 0。
pub fn stack_high_water() -> usize {
    // SAFETY: 只读自己的静态栈区间。
    unsafe {
        let lo = core::ptr::addr_of!(STACK_BOTTOM) as usize;
        let hi = core::ptr::addr_of!(STACK_TOP) as usize;
        let mut p = lo;
        while p < hi {
            if core::ptr::read_volatile(p as *const u64) != 0 {
                break;
            }
            p += 8;
        }
        hi - p
    }
}

/// 当前内核线程栈的用量高水位（字节）。栈来自静态池，`head.S` 已把整个
/// `.bss` 清零过，所以「从栈底往上第一个非零字」就是历史最深处。
///
/// 0 表示当前任务用的是 `head.S` 的静态栈（task[0]），见
/// [`stack_high_water`]。
pub fn kstack_high_water() -> usize {
    // SAFETY: 只读自己的内核栈区间。
    unsafe {
        let lo = current().kernel_stack as usize;
        if lo == 0 {
            return 0;
        }
        let hi = lo + KSTACK_SIZE;
        // 栈底头 8 字节是 STACK_MAGIC，从它之后开始找
        let mut p = lo + 8;
        while p < hi {
            if core::ptr::read_volatile(p as *const u64) != 0 {
                break;
            }
            p += 8;
        }
        hi - p
    }
}

/// 打印全部任务。对应原版 `sched.c:show_state()`。
pub fn show_state() {
    crate::pr!(Level::Info, "sched: jiffies={} switches={} current={}",
               jiffies(), context_switches(), current_nr());
    // SAFETY: 只读任务表。
    let tasks = unsafe { &*core::ptr::addr_of!(TASKS) };
    for (nr, t) in tasks.iter().enumerate() {
        if t.state != TaskState::Unused {
            t.show(nr);
        }
    }
}

/// 当前时间（Unix 纪元秒）。对应原版 `include/linux/sched.h` 的
/// `CURRENT_TIME` 宏（`xtime.tv_sec`），由 `kernel/time.c` 在启动时
/// 从 CMOS RTC 读出 `startup_time` 再加上 `jiffies/HZ`。
///
/// `kernel/time.c` 的 CMOS 读取还没移植，所以 `STARTUP_TIME` 目前是 0，
/// 时间戳等于开机以来的秒数。文件系统只把它当单调递增的戳用
/// （比较新旧、写进 inode），0 起点不影响正确性；接上 RTC 之后
/// 只需给 `STARTUP_TIME` 赋值。
pub fn current_time() -> u32 {
    // SAFETY: 只读一个 u32，启动期设定后不再变。
    let base = unsafe { *core::ptr::addr_of!(STARTUP_TIME) };
    base + (jiffies() / HZ) as u32
}

/// 开机时刻的 Unix 时间。对应原版 `kernel/time.c` 的 `startup_time`。
static mut STARTUP_TIME: u32 = 0;

/// 设置开机时刻。对应原版 `time_init()` 里 `startup_time = mktime(...)`。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn set_startup_time(t: u32) {
    // SAFETY: 契约保证独占。
    unsafe { *core::ptr::addr_of_mut!(STARTUP_TIME) = t }
}

/// 把 PID 计数器清零。
///
/// init 进程（第一个用户态进程）**必须**是 pid 1——busybox init 和
/// sysvinit 都硬检查 `getpid() == 1`。而 boot 期那些 selftest 内核线程
/// （sched 的 worker 等）已经抢先占用了 pid 1/2，它们退出后槽位虽然
/// 释放，`LAST_PID` 却没回退。所以在创建 init 之前把计数器归零，
/// 让 `find_empty_process` 下一次分配拿到 pid 1。
///
/// # Safety
/// 必须在「没有其他任务持有 1..=N 的 pid」且单线程（或关中断）时调用。
/// boot 期在创建 init 前调用满足该前提。
pub fn reset_last_pid() {
    // SAFETY: 契约由调用者保证（boot 期单线程）。
    unsafe { *core::ptr::addr_of_mut!(LAST_PID) = 0; }
}

/// 分配一个新的 PID
/// 
/// 对应原版 `fork.c` 中的 `last_pid` 分配逻辑。
/// 返回一个新分配的 PID，确保在同一时刻不会有两个任务使用相同的 PID。
pub fn allocate_pid() -> i32 {
    // SAFETY: LAST_PID 是原子性操作的全局变量
    // 在单核环境下，调度器在修改 LAST_PID 时会关闭中断
    unsafe {
        let mut pid = *core::ptr::addr_of!(LAST_PID);
        
        // 循环查找未使用的 PID
        // 跳过已使用的 PID
        let max_pid = (1 << 16) as i32; // 限制 PID 范围
        let mut attempts = 0;
        
        loop {
            pid += 1;
            if pid >= max_pid {
                pid = 1; // 从 1 开始，0 保留给特殊用途
            }
            
            // 检查是否有任务使用这个 PID
            let mut in_use = false;
            for i in 0..NR_TASKS {
                if i != current_nr() && (*core::ptr::addr_of_mut!(TASKS))[i].pid == pid {
                    in_use = true;
                    break;
                }
            }
            
            if !in_use {
                *core::ptr::addr_of_mut!(LAST_PID) = pid;
                return pid;
            }
            
            attempts += 1;
            if attempts > max_pid {
                // 应该不会发生
                return -1;
            }
        }
    }
}
