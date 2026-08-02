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
use task::{HZ, NR_TASKS, STACK_MAGIC, Task, TaskState, flags};

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
pub unsafe fn task(nr: usize) -> &'static mut Task {
    // SAFETY: 契约保证下标有界。
    unsafe { &mut (*core::ptr::addr_of_mut!(TASKS))[nr] }
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
            tasks[next].tss.cr3,
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
    }

    // 切页表。目前所有任务共用内核页表（cr3 相同），所以实际上是 noop；
    // 等 fork 真正复制页表后这里才会生效。原版对应 `tss.cr3`。
    if next_cr3 != 0 {
        // SAFETY: 读 cr3 合法；只在值不同时才写，避免无谓的 TLB 全刷。
        unsafe {
            let cur_cr3: u64;
            core::arch::asm!("mov {}, cr3", out(reg) cur_cr3,
                             options(nomem, nostack, preserves_flags));
            if cur_cr3 != next_cr3 {
                core::arch::asm!("mov cr3, {}", in(reg) next_cr3,
                                 options(nostack, preserves_flags));
            }
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

    // SAFETY: current 在中断里读是安全的（调度只发生在中断返回路径上）。
    let cur = unsafe { current() };

    // 原版：`if ((VM_MASK & regs->eflags) || (3 & regs->cs))` 判断被打断的
    // 是用户态还是内核态，据此计入 utime 或 stime。
    if regs.from_user() {
        cur.utime += 1;
    } else {
        cur.stime += 1;
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
        self.head >= NR_TASKS
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
            (*core::ptr::addr_of_mut!(WAIT_NEXT))[nr] = self.head;
        }
        self.head = nr;

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
            if self.head == nr {
                self.head = next[nr];
            } else {
                let mut p = self.head;
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
            let mut p = self.head;
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
pub fn kernel_thread(name: &str, entry: fn(u64), arg: u64, priority: i64) -> KResult<usize> {
    // SAFETY: 全程关中断，独占任务表。
    let flags = unsafe { irq::local_irq_save() };
    let result = (|| -> KResult<usize> {
        // SAFETY: 已关中断。
        let nr = unsafe { find_empty_process()? };

        // 内核栈：两页。原版 `sys_fork` 用 `get_free_page()` 取一页做
        // `kernel_stack_page`，我们要两页所以取两次相邻的做不到——
        // 改用 kmalloc（原版的 kmalloc 上限是 4096-ish，我们的支持到一页；
        // 8192 超了，所以分两页并要求它们相邻）。
        // 简化：直接取两个独立页，只用高的那个当栈，低的那个放魔数区。
        // 实际上一页 4KB 对内核线程够用，跟原版一致，所以只取一页。
        let stack = mm::get_free_page();
        if stack == 0 {
            return Err(EAGAIN);
        }

        // 栈底放魔数，对应原版 `*(unsigned long *)p->kernel_stack_page = STACK_MAGIC`
        // SAFETY: stack 是刚分配、我们独占的整页。
        unsafe { core::ptr::write_volatile(stack as *mut u64, STACK_MAGIC) };

        let stack = stack as u64;
        let stack_top = stack + mm::PAGE_SIZE as u64;

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
            // 共用内核页表（原版 fork 会 copy_page_tables 出一份新的）
            t.tss.cr3 = 0;

            // 挂进调度环。原版 `SET_LINKS(p)` 操作 next_task/prev_task 指针，
            // 我们操作下标，语义相同。
            let cur = current_nr();
            let old_next = task(cur).next;
            task(nr).next = old_next;
            task(nr).prev = cur;
            task(cur).next = nr;
            task(old_next).prev = nr;
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
        let (p, n) = (t.prev, t.next);
        task(p).next = n;
        task(n).prev = p;
        t.state = TaskState::Unused;
        t.kernel_stack = 0;
        irq::restore_flags(flags);

        // 注意：此刻我们还在这个栈上跑，不能立刻释放它。
        // 让 schedule 切走后由下一个任务代为释放——简化处理：
        // 内核线程退出属于自检路径，这里泄漏一页可接受，记在 TODO。
        // 真正的做法是原版那样进 ZOMBIE 由 release() 回收。
        let _ = stack;

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
