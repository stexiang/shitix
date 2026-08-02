//! 进程控制块。对应 linux-1.0.9 的 `include/linux/sched.h` 里的
//! `struct task_struct` / `struct tss_struct` / `INIT_TASK`。
//!
//! 原版 `task_struct` 有近 70 个字段，覆盖信号、文件系统、内存映射、
//! ptrace、itimer、资源限制、i387 状态等等。绝大部分依赖尚未移植的子系统
//! （`fs/`、`kernel/signal.c`、`mm/mmap.c`），塞进来只会是一堆永远为 0 的字段。
//! 所以这里只保留**调度器自身需要**的部分，并在字段上标注原版名字，
//! 后续移植对应子系统时按需补齐。
//!
//! 已保留：state / counter / priority / pid / comm / 亲子链 / 时间统计 / 内核栈。
//! 暂缺（有原版字段名可查）：`signal`/`blocked`/`sigaction[32]`、`filp[NR_OPEN]`、
//! `pwd`/`root`/`executable`、`mmap`、`rlim[]`、`i387`、`ldt`、`debugreg[8]`。

use crate::klib::printk::Level;

/// 最大任务数。对应原版 `include/linux/tasks.h` 的 `NR_TASKS 128`。
/// 我们暂时只需要少量任务，取 16 省一点 BSS。
pub const NR_TASKS: usize = 16;

/// 进程名长度。对应原版 `char comm[16]`。
pub const COMM_LEN: usize = 16;

/// 时钟频率。对应原版 `include/linux/sched.h` 的 `#define HZ 100`。
pub const HZ: u64 = 100;

/// 进程状态。数值与原版 `sched.h` 的 `TASK_*` 宏一致。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i64)]
pub enum TaskState {
    /// 可运行 / 正在运行
    Running = 0,
    /// 可被信号打断的睡眠
    Interruptible = 1,
    /// 不可打断的睡眠
    Uninterruptible = 2,
    /// 已退出，等父进程收尸
    Zombie = 3,
    /// 被 SIGSTOP 停住
    Stopped = 4,
    /// 正在被换出（原版 `TASK_SWAPPING`）
    Swapping = 5,
    /// 该槽位空闲。原版用 `task[n] == NULL` 表示，我们是定长数组所以要个状态位。
    Unused = 6,
}

/// 每进程标志。取值同原版 `sched.h` 的 `PF_*`。
pub mod flags {
    /// 打印对齐警告（原版说 "Not implemented yet"）
    pub const PF_ALIGNWARN: u64 = 0x0000_0001;
    /// 已被 ptrace 附加
    pub const PF_PTRACED: u64 = 0x0000_0010;
    /// 正在跟踪系统调用
    pub const PF_TRACESYS: u64 = 0x0000_0020;
    /// 内核线程。原版没有这个概念（1.0.9 的 init 是 fork+execve 出来的），
    /// 我们需要它来区分「不返回用户态」的任务。
    pub const PF_KTHREAD: u64 = 0x0000_1000;
}

/// 内核栈魔数。对应原版 `include/linux/kernel.h` 的 `STACK_MAGIC 0xdeadbeef`，
/// 放在内核栈页最低地址，`die_if_kernel` 靠它检测栈溢出。
pub const STACK_MAGIC: u64 = 0xdead_beef;

/// 每个任务的内核栈大小。原版是一整页（`kernel_stack_page`），
/// 64 位下栈帧更大（每个寄存器 8 字节、pt_regs 就 0xa8），给两页。
pub const KERNEL_STACK_SIZE: usize = 8192;

/// 切换现场。对应原版 `struct tss_struct`，但只剩两个字段：
///
/// long mode 取消了 TSS 硬件任务切换，`switch_to` 变成软件保存
/// callee-saved 寄存器到栈上、再换 `rsp`（见 `boot/entry.S:switch_to`）。
/// 所以「现场」只需要记住栈顶在哪；寄存器都在那个栈上。
/// 原版那些 `eax`/`ecx`/`es`/`ldt`/`io_bitmap`/`i387` 字段全部不需要。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Tss {
    /// 内核栈指针。switch_to 从这里恢复（原版 `tss.esp`）
    pub rsp: u64,
    /// 页表根。原版 `tss.cr3`，目前所有任务共用内核页表所以恒等
    pub cr3: u64,
    /// 内核栈顶，用于写 TSS.rsp0（原版 `tss.esp0`）
    pub rsp0: u64,
    /// 最近一次异常的向量号（原版 `tss.trap_no`）
    pub trap_no: u64,
    /// 最近一次异常的错误码（原版 `tss.error_code`）
    pub error_code: u64,
    /// page fault 的出错地址（原版 `tss.cr2`）
    pub cr2: u64,
}

impl Tss {
    const fn new() -> Self {
        Tss { rsp: 0, cr3: 0, rsp0: 0, trap_no: 0, error_code: 0, cr2: 0 }
    }
}

/// 进程控制块。对应原版 `struct task_struct`（见模块文档说明取舍）。
#[repr(C)]
pub struct Task {
    // ---- 原版注释说 "these are hardcoded - don't touch" 的那几个 ----
    /// 原版 `volatile long state`
    pub state: TaskState,
    /// 剩余时间片。原版 `long counter`
    pub counter: i64,
    /// 时间片基准（nice 值的反面）。原版 `long priority`
    pub priority: i64,
    /// 待处理信号位图。原版 `unsigned long signal`
    pub signal: u64,
    /// 被屏蔽的信号。原版 `unsigned long blocked`
    pub blocked: u64,
    /// 原版 `unsigned long flags`，取值见 [`flags`]
    pub flags: u64,
    /// 原版 `int errno`：系统调用把错误码存这里，返回路径取反后送 rax
    pub errno: i32,

    // ---- 标识 ----
    /// 原版 `int pid`
    pub pid: i32,
    /// 进程组。原版 `int pgrp`
    pub pgrp: i32,
    /// 会话。原版 `int session`
    pub session: i32,
    /// 原版 `char comm[16]`
    pub comm: [u8; COMM_LEN],

    // ---- 亲子链。原版 `p_opptr/p_pptr/p_cptr/p_ysptr/p_osptr` ----
    /// 父进程在 task 数组里的下标（原版是指针，我们用下标避免自引用结构）
    pub parent: usize,

    // ---- 调度链。原版 `next_task`/`prev_task` 双向环 ----
    /// 下一个任务的下标，构成环。原版 `struct task_struct *next_task`
    pub next: usize,
    /// 上一个任务的下标。原版 `prev_task`
    pub prev: usize,

    // ---- 时间统计 ----
    /// 用户态累计滴答。原版 `long utime`
    pub utime: u64,
    /// 内核态累计滴答。原版 `long stime`
    pub stime: u64,
    /// 创建时刻的 jiffies。原版 `long start_time`
    pub start_time: u64,
    /// 睡眠超时时刻。原版 `unsigned long timeout`
    pub timeout: u64,

    // ---- 内核栈与切换现场 ----
    /// 内核栈页的基址（最低地址）。原版 `unsigned long kernel_stack_page`
    pub kernel_stack: u64,
    /// 原版 `struct tss_struct tss`
    pub tss: Tss,

    /// 退出码。原版 `int exit_code`
    pub exit_code: i32,
}

impl Task {
    /// 一个空槽位。
    pub const fn empty() -> Self {
        Task {
            state: TaskState::Unused,
            counter: 0,
            priority: 15,
            signal: 0,
            blocked: 0,
            flags: 0,
            errno: 0,
            pid: 0,
            pgrp: 0,
            session: 0,
            comm: [0; COMM_LEN],
            parent: 0,
            next: 0,
            prev: 0,
            utime: 0,
            stime: 0,
            start_time: 0,
            timeout: 0,
            kernel_stack: 0,
            tss: Tss::new(),
            exit_code: 0,
        }
    }

    /// 进程名，遇 NUL 截断。
    pub fn name(&self) -> &str {
        let n = self.comm.iter().position(|&b| b == 0).unwrap_or(COMM_LEN);
        core::str::from_utf8(&self.comm[..n]).unwrap_or("<bad comm>")
    }

    /// 设置进程名，超长截断。
    pub fn set_name(&mut self, name: &str) {
        self.comm = [0; COMM_LEN];
        let src = name.as_bytes();
        let n = src.len().min(COMM_LEN - 1);
        self.comm[..n].copy_from_slice(&src[..n]);
    }

    /// 是否内核线程。
    #[inline]
    pub fn is_kthread(&self) -> bool {
        self.flags & flags::PF_KTHREAD != 0
    }

    /// 有没有未被屏蔽的待处理信号。对应原版
    /// `ret_from_sys_call` 里那句 `notl %ecx; andl signal(%eax),%ecx`。
    #[inline]
    pub fn has_pending_signal(&self) -> bool {
        self.signal & !self.blocked != 0
    }

    /// 内核栈顶（栈向下增长，所以是基址 + 大小）。
    #[inline]
    pub fn stack_top(&self) -> u64 {
        self.kernel_stack + KERNEL_STACK_SIZE as u64
    }

    /// 检查栈底的魔数是否还在。对应原版 die_if_kernel 里那句
    /// `if (STACK_MAGIC != *(unsigned long *)current->kernel_stack_page)`。
    pub fn stack_ok(&self) -> bool {
        if self.kernel_stack == 0 {
            return true; // init_task 用的是 head.S 的静态栈，没有魔数
        }
        // SAFETY: kernel_stack 指向本任务独占的、已分配的内核栈页。
        unsafe { core::ptr::read_volatile(self.kernel_stack as *const u64) == STACK_MAGIC }
    }

    /// 打印一行摘要。对应原版 `show_task()`。
    pub fn show(&self, nr: usize) {
        crate::pr!(Level::Info,
                   "  [{}] pid={} {:8} state={:?} counter={} pri={} utime={} stime={} stack={:#x}",
                   nr, self.pid, self.name(), self.state, self.counter,
                   self.priority, self.utime, self.stime, self.kernel_stack);
    }
}
