//! 信号处理。参考 linux-1.0.9 的 `kernel/signal.c`。
//!
//! ## 功能
//!
//! - 信号定义（SIGHUP ~ SIGSYS）
//! - 信号发送（send_sig）
//! - 信号掩码操作（sigprocmask）
//! - 信号待处理查询（sigpending）
//! - 信号处理安装（signal/sigaction）
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `signal.c` | 信号核心实现 |
//! | `signal.h` | 信号定义 |
//! | `sched.h` | task_struct 里的 signal/blocked 字段 |

// Re-export signals for use in other modules
pub use crate::traps::signal::*;

// =============================================================================
// Signal Numbers
// =============================================================================

/// 信号号。对应 `include/linux/signal.h`。
///
/// Linux 1.0.9 定义了 31 个标准信号（SIGRTMIN 之前）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Signal {
    /// Hangup - 终端挂起或控制进程终止
    SIGHUP = 1,
    /// Interrupt - 来自键盘的中断 (Ctrl+C)
    SIGINT = 2,
    /// Quit - 来自键盘的退出 (Ctrl+\)
    SIGQUIT = 3,
    /// Illegal instruction
    SIGILL = 4,
    /// Trace/breakpoint trap
    SIGTRAP = 5,
    /// Abort
    SIGABRT = 6,
    /// Bus error
    SIGBUS = 7,
    /// Floating point exception
    SIGFPE = 8,
    /// Kill - 强制终止
    SIGKILL = 9,
    /// User defined signal 1
    SIGUSR1 = 10,
    /// Segmentation fault
    SIGSEGV = 11,
    /// User defined signal 2
    SIGUSR2 = 12,
    /// Write on pipe with no readers
    SIGPIPE = 13,
    /// Alarm clock
    SIGALRM = 14,
    /// Termination
    SIGTERM = 15,
    /// Stack fault on coprocessor
    SIGSTKFLT = 16,
    /// Child stopped or terminated
    SIGCHLD = 17,
    /// Continue if stopped
    SIGCONT = 18,
    /// Stop (process)
    SIGSTOP = 19,
    /// Stop typed at terminal
    SIGTSTP = 20,
    /// Terminal input for background job
    SIGTTIN = 21,
    /// Terminal output for background job
    SIGTTOU = 22,
    /// Urgent data on socket
    SIGURG = 23,
    /// CPU time limit exceeded
    SIGXCPU = 24,
    /// File size limit exceeded
    SIGXFSZ = 25,
    /// Virtual alarm clock
    SIGVTALRM = 26,
    /// Profiling alarm clock
    SIGPROF = 27,
    /// Window resize
    SIGWINCH = 28,
    /// I/O now possible
    SIGIO = 29,
    /// Power failure
    SIGPWR = 30,
    /// Bad system call
    SIGSYS = 31,
}

impl Signal {
    /// 从 u32 值转换为 Signal（如果不是有效信号返回 None）
    pub fn from_u32(nr: u32) -> Option<Signal> {
        match nr {
            1 => Some(Signal::SIGHUP),
            2 => Some(Signal::SIGINT),
            3 => Some(Signal::SIGQUIT),
            4 => Some(Signal::SIGILL),
            5 => Some(Signal::SIGTRAP),
            6 => Some(Signal::SIGABRT),
            7 => Some(Signal::SIGBUS),
            8 => Some(Signal::SIGFPE),
            9 => Some(Signal::SIGKILL),
            10 => Some(Signal::SIGUSR1),
            11 => Some(Signal::SIGSEGV),
            12 => Some(Signal::SIGUSR2),
            13 => Some(Signal::SIGPIPE),
            14 => Some(Signal::SIGALRM),
            15 => Some(Signal::SIGTERM),
            16 => Some(Signal::SIGSTKFLT),
            17 => Some(Signal::SIGCHLD),
            18 => Some(Signal::SIGCONT),
            19 => Some(Signal::SIGSTOP),
            20 => Some(Signal::SIGTSTP),
            21 => Some(Signal::SIGTTIN),
            22 => Some(Signal::SIGTTOU),
            23 => Some(Signal::SIGURG),
            24 => Some(Signal::SIGXCPU),
            25 => Some(Signal::SIGXFSZ),
            26 => Some(Signal::SIGVTALRM),
            27 => Some(Signal::SIGPROF),
            28 => Some(Signal::SIGWINCH),
            29 => Some(Signal::SIGIO),
            30 => Some(Signal::SIGPWR),
            31 => Some(Signal::SIGSYS),
            _ => None,
        }
    }

    /// 转换为 u32
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    /// 获取信号对应的位掩码。
    ///
    /// **位号 == 信号号**（bit 0 空着不用），而不是原版的 `1 << (sig-1)`。
    /// 原版的 `signal`/`blocked` 是 32 位，31 个信号刚好铺满，只能从 bit 0
    /// 起算；我们是 64 位，让位号直接等于信号号能省掉满地的 ±1，
    /// `do_signal` 里 `trailing_zeros()` 拿到的就是信号号。
    ///
    /// 这个约定必须和 [`send_sig`]、[`dequeue_signal`]、[`sigaddset`]、
    /// [`BLOCKABLE`] 保持一致——曾经 `mask()` 用 `sig-1` 而 `send_sig` 用
    /// `sig`，导致 `blocked` 和 `signal` 对不上、屏蔽字形同虚设。
    pub fn mask(self) -> u64 {
        1u64 << (self as u8)
    }
}

/// 信号处理函数类型
pub type SignalHandler = Option<extern "C" fn(u32)>;

/// 信号处理标志。对应 `SA_*`。
#[derive(Clone, Copy, Debug, Default)]
#[repr(transparent)]
pub struct SigActionFlags(u64);

impl SigActionFlags {
    /// 使用旧的 signal() 语义
    pub const SA_NOCLDSTOP: u64 = 1;
    /// 不阻塞等待的子进程
    pub const SA_NOCLDWAIT: u64 = 2;
    /// 信号处理函数使用系统调用栈
    pub const SA_SIGINFO: u64 = 4;
    /// 不自动重启被中断的系统调用
    pub const SA_RESTART: u64 = 8;
    /// 停止对 SIGSTOP 的默认处理
    pub const SA_STOPSIG: u64 = 0x10;
    /// 重置动作为默认
    pub const SA_RESETHAND: u64 = 0x20;

    /// 是否为空
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// 获取原始值
    pub const fn bits(&self) -> u64 {
        self.0
    }

    /// 从原始值创建
    pub const fn from_bits(bits: u64) -> Self {
        SigActionFlags(bits)
    }
}

/// 信号处理动作
#[derive(Clone, Copy)]
#[repr(C)]
pub struct SigAction {
    /// 处理函数
    pub handler: SignalHandler,
    /// 旧信号掩码（原版 sa_mask）
    pub mask: u64,
    /// 标志（原版 sa_flags）
    pub flags: SigActionFlags,
    /// 恢复函数（原版 sa_restorer）
    pub restorer: Option<extern "C" fn()>,
}

impl SigAction {
    /// 创建默认动作（终止进程）
    pub const fn default() -> Self {
        SigAction {
            handler: None,
            mask: 0,
            flags: SigActionFlags(0),
            restorer: None,
        }
    }

    /// 创建忽略动作
    pub const fn ignore() -> Self {
        SigAction {
            handler: Some(ignore_handler),
            mask: 0,
            flags: SigActionFlags(0),
            restorer: None,
        }
    }
}

/// 默认的忽略处理函数
extern "C" fn ignore_handler(_signum: u32) {}

/// SIG_DFL - 默认动作
pub const SIG_DFL: SignalHandler = None;
/// SIG_IGN - 忽略信号
pub const SIG_IGN: SignalHandler = Some(ignore_handler);
/// SIG_ERR - 错误返回值
pub const SIG_ERR: SignalHandler = None;

// =============================================================================
// Signal Set Operations
// =============================================================================

/// 信号集大小（64 位，所以最多 64 个信号）
pub const SIGSET_SIZE: usize = 8;

/// 信号集类型。对应 `sigset_t`。
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct SigSet {
    /// 位掩码数组
    pub bits: [u64; SIGSET_SIZE],
}

impl SigSet {
    /// 创建空信号集
    pub const fn empty() -> Self {
        SigSet { bits: [0; SIGSET_SIZE] }
    }

    /// 创建全部信号集
    pub const fn full() -> Self {
        SigSet { bits: [!0u64; SIGSET_SIZE] }
    }

    /// 添加信号到集合
    pub fn add(&mut self, sig: Signal) {
        let bit = sig.mask();
        self.bits[0] |= bit;
    }

    /// 从集合删除信号
    pub fn del(&mut self, sig: Signal) {
        let bit = sig.mask();
        self.bits[0] &= !bit;
    }

    /// 检查信号是否在集合中
    pub fn has(&self, sig: Signal) -> bool {
        (self.bits[0] & sig.mask()) != 0
    }

    /// 清空集合
    pub fn clear(&mut self) {
        self.bits = [0; SIGSET_SIZE];
    }

    /// 填充集合
    pub fn fill(&mut self) {
        self.bits = [!0u64; SIGSET_SIZE];
    }

    /// 阻塞集合（与当前 blocked 进行与操作）
    pub fn and_blocked(&self, blocked: u64) -> SigSet {
        let mut result = *self;
        result.bits[0] &= blocked;
        result
    }
}

/// 可阻塞信号掩码（SIGKILL 和 SIGSTOP 不能被阻塞）。
/// 位号约定见 [`Signal::mask`]：位号 == 信号号。
pub const BLOCKABLE: u64 =
    !((1u64 << (Signal::SIGKILL as u8)) | (1u64 << (Signal::SIGSTOP as u8)));

// =============================================================================
// Signal Sending
// =============================================================================

use crate::sched::{self, task::TaskState};

/// 向进程发送信号。参考 `kernel/signal.c:send_sig()`。
///
/// # Arguments
///
/// * `signum` - 信号号
/// * `task_idx` - 目标任务的索引
/// * `_priv` - 权限级别（0=普通用户，1=内核）
///
/// # Returns
///
/// * 0 - 成功
/// * -1 - 无效任务
/// * -EINVAL - 无效信号
pub fn send_sig(signum: u32, task_idx: usize, _priv: i32) -> i32 {
    // 验证信号号
    if signum == 0 {
        return 0; // 信号 0 用于检查进程存在性
    }
    if signum > 31 {
        return -1;
    }

    // SAFETY: 调用者持有调度器锁，或者在紧急情况下
    unsafe {
        let task = sched::task_ptr(task_idx);
        
        // 检查任务是否存在
        if (*task).state == TaskState::Unused {
            return -1;
        }

        // 权限检查：非特权只能向自己的进程组发送信号
        // TODO: 实现完整的权限检查

        // ---- 原版 generate() 的过滤，别省 ----
        // `kernel/signal.c:generate()` 在置位**之前**会先把「反正不会有动作」
        // 的信号丢掉：
        //   - SIG_IGN 且不是 SIGCHLD          → 直接丢
        //   - SIG_DFL 且 ∈{SIGCHLD,SIGCONT,SIGWINCH} → 直接丢（默认忽略）
        //
        // 这不是优化而是语义：`sys_wait4` 睡在 Interruptible 上，醒来只要看到
        // 任何未屏蔽的待处理信号就返回 -EINTR。notify_parent 给父进程发的正是
        // SIGCHLD，如果这里置了位，父进程的 wait4 会被自己等的那个孩子打断，
        // 永远收不到尸。
        let action = get_task_signal(task_idx, signum as usize);
        let is_chld = signum == Signal::SIGCHLD as u32;
        match action.handler {
            Some(h) => {
                let ign: extern "C" fn(u32) = ignore_handler;
                if h as usize == ign as usize && !is_chld {
                    return 0;
                }
            }
            None => {
                // SIG_DFL：默认忽略的那三个不置位。
                if is_chld
                    || signum == Signal::SIGCONT as u32
                    || signum == Signal::SIGWINCH as u32
                {
                    // SIGCONT 还有个副作用：原版在 send_sig 里就把 STOPPED
                    // 的任务放回运行态（不经过 do_signal）。
                    if signum == Signal::SIGCONT as u32
                        && (*task).state == TaskState::Stopped
                    {
                        (*task).state = TaskState::Running;
                    }
                    return 0;
                }
            }
        }

        // 设置信号位。位号 == 信号号（见模块里 sigmask 约定的说明）。
        (*task).signal |= 1u64 << signum;

        // 如果任务处于可中断睡眠状态，唤醒它
        if (*task).state == TaskState::Interruptible {
            (*task).state = TaskState::Running;
        }

        0
    }
}

/// 向当前任务发送信号
pub fn send_sig_current(signum: u32) -> i32 {
    let current = sched::current_index();
    send_sig(signum, current, 0)
}

/// 强制终止任务
pub fn force_sig(signum: u32, task_idx: usize) -> i32 {
    // force_sig 不做权限检查
    if signum > 31 {
        return -1;
    }
    // SAFETY: 调用者持有调度器锁
    unsafe {
        let task = sched::task_ptr(task_idx);
        (*task).signal |= 1u64 << signum;
        0
    }
}

// =============================================================================
// Signal Mask Operations
// =============================================================================

/// 获取当前阻塞信号掩码
pub fn sigmask() -> u64 {
    // SAFETY: 只读当前任务
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        (*task).blocked
    }
}

/// 设置阻塞信号掩码（返回旧值）
pub fn setsigmask(new_mask: u64) -> u64 {
    // SAFETY: 修改当前任务的 blocked
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        let old = (*task).blocked;
        (*task).blocked = new_mask & BLOCKABLE;
        old
    }
}

/// 添加信号到阻塞集
pub fn sigaddset(sig: u32) -> i32 {
    if sig > 31 {
        return -1;
    }
    // SAFETY: 修改当前任务的 blocked
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        (*task).blocked |= 1u64 << sig;
    }
    0
}

/// 从阻塞集删除信号
pub fn sigdelset(sig: u32) -> i32 {
    if sig > 31 {
        return -1;
    }
    // SAFETY: 修改当前任务的 blocked
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        (*task).blocked &= !(1u64 << sig);
    }
    0
}

/// 清除所有阻塞信号
pub fn sigemptyset() {
    // SAFETY: 修改当前任务的 blocked
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        (*task).blocked = 0;
    }
}

/// 填充阻塞信号集
pub fn sigfillset() {
    // SAFETY: 修改当前任务的 blocked
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        (*task).blocked = BLOCKABLE;
    }
}

// =============================================================================
// Signal Pending Check
// =============================================================================

/// [`signal_pending`] 的 C ABI 包装，供 `entry.S:ret_from_sys_call` 调用。
///
/// 返回 1 表示当前任务有未屏蔽的待处理信号，需要走 [`do_signal`]。
/// 汇编侧只 `testb %al,%al`，所以用 u8 而不是 bool（bool 的 ABI 保证
/// 只有 0/1，但显式写成 u8 更贴合汇编的读法）。
///
/// # Safety
/// 只能在有当前任务、且 pt_regs 完整的返回路径上调用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn signal_pending_c() -> u8 {
    if signal_pending() { 1 } else { 0 }
}

/// 检查是否有待处理信号（考虑阻塞）
pub fn signal_pending() -> bool {
    // SAFETY: 只读当前任务
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        ((*task).signal & !(*task).blocked) != 0
    }
}

/// 获取待处理信号（考虑阻塞）
pub fn dequeue_signal() -> Option<Signal> {
    // SAFETY: 修改当前任务
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        let pending = (*task).signal & !(*task).blocked;
        
        if pending == 0 {
            return None;
        }

        // 找到最低位的待处理信号
        // 从信号 1 开始查找（信号 0 不存在）
        for sig in 1u32..=31 {
            if (pending & (1u64 << sig)) != 0 {
                // 清除该信号
                (*task).signal &= !(1u64 << sig);
                return Signal::from_u32(sig);
            }
        }
        None
    }
}

// =============================================================================
// Signal Actions
// =============================================================================

/// 每个任务的信号处理动作表
/// 对应原版 `current->sigaction[32]`
const NR_SIGACTIONS: usize = 32;

/// 每任务的 sigaction 表指针，0 表示「这个任务还没装过任何 handler」。
///
/// 原版把 `struct sigaction sigaction[32]` 内联在 `task_struct` 里（整个
/// task_struct 本身占一页）。我们不能照抄：`SigAction` 32 字节 × 32 信号
/// × `NR_TASKS` = 16KB **静态** BSS，而 `_kernel_end` 必须 < 0x90000
/// （见 `boot/kernel.ld` 的 ASSERT），16KB 一口气吃掉全部余量。
///
/// 所以改成**按需分页**：绝大多数任务从生到死都不调 sigaction()，
/// 全默认动作不需要任何存储。真装了 handler 才分配一页（1KB 表放得下），
/// 在 [`reset_sigactions`] 里还回去。代价是 128 字节指针数组。
static mut SIGACTION_TABLES: [usize; crate::sched::NR_TASKS] = [0; crate::sched::NR_TASKS];

/// 一张 sigaction 表的字节数。必须 <= 一页，否则 get_free_page 不够用。
const SIGACTION_TABLE_BYTES: usize = NR_SIGACTIONS * core::mem::size_of::<SigAction>();
const _: () = assert!(SIGACTION_TABLE_BYTES <= 4096, "sigaction 表超过一页");

/// 取某个任务的 sigaction 表首指针，没分配过就返回 null。
///
/// # Safety
/// 调用者必须独占任务表（关中断或单核不抢占）。
unsafe fn table_ptr(task_idx: usize) -> *mut SigAction {
    // SAFETY: 契约转交；下标由调用方查界。
    unsafe { SIGACTION_TABLES[task_idx] as *mut SigAction }
}

/// 取某个任务的 sigaction 表，没有就分配并全部初始化成 SIG_DFL。
/// 分配失败返回 null（调用方退化成「保持默认动作」）。
///
/// # Safety
/// 同 [`table_ptr`]。
unsafe fn table_ptr_or_alloc(task_idx: usize) -> *mut SigAction {
    // SAFETY: 契约转交。
    unsafe {
        let existing = table_ptr(task_idx);
        if !existing.is_null() {
            return existing;
        }
        let page = crate::mm::get_free_page();
        if page == 0 {
            crate::pr_warn!("signal: no page for task {}'s sigaction table", task_idx);
            return core::ptr::null_mut();
        }
        let p = page as *mut SigAction;
        for i in 0..NR_SIGACTIONS {
            core::ptr::write(p.add(i), SigAction::default());
        }
        SIGACTION_TABLES[task_idx] = page;
        p
    }
}

/// 获取当前任务的信号处理动作
pub fn get_signal(signum: usize) -> SigAction {
    get_task_signal(sched::current_index(), signum)
}

/// 获取指定任务的信号处理动作。没装过 handler 的任务一律 SIG_DFL。
pub fn get_task_signal(task_idx: usize, signum: usize) -> SigAction {
    if signum >= NR_SIGACTIONS || task_idx >= sched::NR_TASKS {
        return SigAction::default();
    }
    // SAFETY: 下标已查界；单核不抢占，这段没有并发写。
    unsafe {
        let p = table_ptr(task_idx);
        if p.is_null() {
            return SigAction::default();
        }
        core::ptr::read(p.add(signum))
    }
}

/// 设置当前任务的信号处理动作
pub fn set_signal(signum: usize, action: SigAction) {
    set_task_signal(sched::current_index(), signum, action);
}

/// 设置指定任务的信号处理动作。首次调用会为该任务分配一页表。
pub fn set_task_signal(task_idx: usize, signum: usize, action: SigAction) {
    if signum >= NR_SIGACTIONS || task_idx >= sched::NR_TASKS {
        return;
    }
    // SAFETY: 下标已查界。调用者在系统调用上下文里（单核不抢占）。
    unsafe {
        let p = table_ptr_or_alloc(task_idx);
        if p.is_null() {
            return; // 分配失败：保持默认动作
        }
        core::ptr::write(p.add(signum), action);
    }
}

/// 把某个任务的 sigaction 表全部复位成 SIG_DFL，并把页还给分配器。
/// 对应原版 `exec` 里那段「非 SIG_IGN 的 handler 全部清成 SIG_DFL」，
/// 以及 `release()` 之后槽位复用前的清理。
pub fn reset_sigactions(task_idx: usize) {
    if task_idx >= sched::NR_TASKS {
        return;
    }
    // SAFETY: 下标已查界。
    unsafe {
        let page = SIGACTION_TABLES[task_idx];
        if page != 0 {
            SIGACTION_TABLES[task_idx] = 0;
            crate::mm::free_page(page);
        }
    }
}

/// fork 时把父进程的 sigaction 表整份复制给子进程。
/// 对应原版 `copy_process` 里 `*p = *current`（sigaction 是内联数组，随之复制）。
///
/// 父进程没装过 handler 就什么都不用做——子进程同样全默认。
pub fn clone_sigactions(from: usize, to: usize) {
    if from >= sched::NR_TASKS || to >= sched::NR_TASKS || from == to {
        return;
    }
    // SAFETY: 两个下标已查界且互不相同，两张表是分开的页，不重叠。
    unsafe {
        let src = table_ptr(from);
        if src.is_null() {
            // 子进程可能继承了父进程 clone 前的旧表指针（*child = parent.clone()
            // 只复制 Task，不碰这个旁路数组），但槽位在 release 时已清 0，
            // 这里再确认一次，避免子进程误用别人的表。
            SIGACTION_TABLES[to] = 0;
            return;
        }
        let dst = table_ptr_or_alloc(to);
        if dst.is_null() {
            return; // 分配失败：子进程退化成全默认动作
        }
        core::ptr::copy_nonoverlapping(src, dst, NR_SIGACTIONS);
    }
}

// =============================================================================
// Signal Delivery
// =============================================================================

/// 在用户栈上搭信号帧，使 `iretq` 回到用户态时跳转进 handler。
///
/// 帧布局（从高到低）：
///   [原用户栈]
///   [sigreturn 蹦床: mov $15,%eax; int $0x80; ret]    ← 8 B
///   [saved rip] (8 B)
///   [saved cs]  (8 B)
///   [saved rflags] (8 B)
///   [saved rsp] (8 B)
///   [saved ss]  (8 B)                                   ← 新 rsp = 这里
///
/// # Safety
/// `regs` 必须指向当前任务内核栈顶的 pt_regs。
unsafe fn setup_frame(regs: *mut crate::traps::PtRegs, signum: u32, handler: extern "C" fn(u32)) {
    use crate::desc::selector::{USER_CS, USER_DS};

    // SAFETY: 契约转交。
    unsafe {
        let r = &mut *regs;

        // 计算信号帧在用户栈上的位置（rsp 往下放 64 字节）
        let frame_sp = (r.rsp - 64) & !0xF; // 16 字节对齐

        // 1. 写 sigreturn 蹦床到用户栈
        let trampoline: [u8; 8] = [0xb8, 0x0f, 0x00, 0x00, 0x00, // mov eax, 15
                                   0xcd, 0x80,                     // int 0x80
                                   0xc3];                          // ret
        // 蹦床在上，saved regs 在下
        let tramp_addr = frame_sp + 40; // 5 个 saved qwords = 40 字节后
        // SAFETY: 恒等映射可写；用户页表映射了这些页。
        core::ptr::copy_nonoverlapping(trampoline.as_ptr(), tramp_addr as *mut u8, 8);

        // 2. 保存当前 pt_regs 的关键字段到用户栈
        let save = frame_sp as *mut u64;
        core::ptr::write_volatile(save.add(0), r.ss);     // [frame_sp +  0] saved ss
        core::ptr::write_volatile(save.add(1), r.rsp);    // [frame_sp +  8] saved rsp
        core::ptr::write_volatile(save.add(2), r.rflags); // [frame_sp + 16] saved rflags
        core::ptr::write_volatile(save.add(3), r.cs);     // [frame_sp + 24] saved cs
        core::ptr::write_volatile(save.add(4), r.rip);    // [frame_sp + 32] saved rip

        // 3. 改写 pt_regs：下一次 iretq 会跳到 handler
        r.rip = handler as u64;
        r.cs = USER_CS as u64;
        r.rflags = 0x202;  // IF=1
        r.rsp = frame_sp;
        r.ss = USER_DS as u64;
        r.rdi = signum as u64; // 第一个参数 = 信号号
    }
}

/// 投递待处理信号。由 `entry.S:ret_from_sys_call` 在返回用户态前调用，
/// 对应原版 `ret_from_sys_call` 里那句 `call _do_signal`。
///
/// 原版签名是 `do_signal(unsigned long oldmask, struct pt_regs *regs)`，
/// 会在用户栈上搭一个信号帧让用户 handler 跑起来、再靠 `sa_restorer`
/// 调 `sigreturn` 回来。我们还没有用户态进程（execve 未移植），所以
/// **自定义 handler 暂时按默认动作处理**——一旦有了用户栈就在这里补
/// `setup_frame`。SIG_DFL / SIG_IGN 的语义是完整的。
///
/// # Safety
/// 只能由 entry.S 在「即将返回用户态、pt_regs 完整、intr_count == 0」
/// 的位置调用。`regs` 必须指向当前内核栈顶那份 pt_regs。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn do_signal(regs: *mut crate::traps::PtRegs) {
    let nr = sched::current_index();

    // SAFETY: nr 来自 current_index()，槽位必然有效。
    let task = unsafe { sched::task_ptr(nr) };

    loop {
        // SAFETY: task 指向有效槽位；单核不抢占，这段没有并发写。
        let signum = unsafe {
            let pending = (*task).signal & !(*task).blocked;
            if pending == 0 {
                return;
            }
            // 取最低位的待处理信号。信号 n 用 bit n（见 send_sig），bit 0 不用。
            let signum = pending.trailing_zeros();
            // 摘掉这一位再处理，避免默认动作里再进来死循环。
            (*task).signal &= !(1u64 << signum);
            signum
        };

        if signum == 0 || signum > 31 {
            continue;
        }

        let action = get_task_signal(nr, signum as usize);

        // SIG_IGN：丢弃，继续看下一个。
        if let Some(h) = action.handler {
            // 先转 fn 指针再转整数：直接 `ignore_handler as usize` 是把
            // 函数**项**塞进整数，rustc 会警告（拿到的是单态化后的地址，
            // 与 SIG_IGN 里存的那个不保证同一个）。
            let ign: extern "C" fn(u32) = ignore_handler;
            if h as usize == ign as usize {
                continue;
            }
            // 自定义 handler：在用户栈上搭信号帧
            // SAFETY: regs 指向当前内核栈上的 pt_regs。
            unsafe { setup_frame(regs, signum, h) };
            continue; // 已设好帧，返回用户态后 handler 会跑
        }

        // SIG_DFL：按原版 do_signal 的 default 分支分类。
        match signum {
            // 忽略类：SIGCHLD / SIGURG / SIGWINCH
            s if s == Signal::SIGCHLD as u32
                || s == Signal::SIGURG as u32
                || s == Signal::SIGWINCH as u32 =>
            {
                continue;
            }
            // SIGCONT：把被 SIGSTOP 停住的任务放回运行态。
            s if s == Signal::SIGCONT as u32 => {
                // SAFETY: task 有效。
                unsafe {
                    if (*task).state == TaskState::Stopped {
                        (*task).state = TaskState::Running;
                    }
                }
                continue;
            }
            // 停止类：SIGSTOP / SIGTSTP / SIGTTIN / SIGTTOU
            s if s == Signal::SIGSTOP as u32
                || s == Signal::SIGTSTP as u32
                || s == Signal::SIGTTIN as u32
                || s == Signal::SIGTTOU as u32 =>
            {
                // SAFETY: task 有效；随后 schedule() 让出 CPU。
                unsafe {
                    (*task).state = TaskState::Stopped;
                    (*task).exit_code = signum as i32;
                    sched::schedule();
                }
                continue;
            }
            // 其余全部终止。原版对 SIGQUIT/SIGILL/... 还会 dump core，我们没有 core。
            _ => crate::exit::do_exit(signum as i32),
        }
    }
}

/// 初始化信号处理
pub fn init() {
    // 所有任务的所有信号都从 SIG_DFL 开始（大部分默认动作是终止进程）。
    for t in 0..sched::NR_TASKS {
        reset_sigactions(t);
    }

    crate::sprintln!(
        "signal: {} handlers x {} tasks initialized",
        NR_SIGACTIONS,
        sched::NR_TASKS
    );
}


