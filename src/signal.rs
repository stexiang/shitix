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

    /// 获取信号对应的位掩码
    pub fn mask(self) -> u64 {
        1u64 << (self as u8 - 1)
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

/// 可阻塞信号掩码（SIGKILL 和 SIGSTOP 不能被阻塞）
pub const BLOCKABLE: u64 = !(1u64 << (Signal::SIGKILL as u8 - 1) | 1u64 << (Signal::SIGSTOP as u8 - 1));

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

        // 设置信号位
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
/// 对应原版 `current->sigaction[64]`
const NR_SIGACTIONS: usize = 32;

static mut SIG_ACTIONS: [SigAction; NR_SIGACTIONS] = [const { SigAction::default() }; NR_SIGACTIONS];

/// 获取信号处理动作
pub fn get_signal(signum: usize) -> SigAction {
    if signum >= NR_SIGACTIONS {
        return SigAction::default();
    }
    // SAFETY: 只读
    unsafe {
        SIG_ACTIONS[signum]
    }
}

/// 设置信号处理动作
pub fn set_signal(signum: usize, action: SigAction) {
    if signum >= NR_SIGACTIONS {
        return;
    }
    // SAFETY: 修改信号处理表
    unsafe {
        SIG_ACTIONS[signum] = action;
    }
}

/// 初始化信号处理
pub fn init() {
    // 设置默认的 SIG_DFL 和 SIG_IGN
    // 大部分信号默认动作是终止进程
    for i in 0..NR_SIGACTIONS {
        unsafe {
            SIG_ACTIONS[i] = SigAction::default();
        }
    }

    // SIGCHLD 默认识别（不产生僵尸）
    // TODO: 实现 SIGCHLD 的特殊处理

    crate::sprintln!("signal: {} signal handlers initialized", NR_SIGACTIONS);
}


