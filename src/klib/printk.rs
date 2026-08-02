//! 内核日志。对应 linux-1.0.9 的 `kernel/printk.c` +
//! `include/linux/kernel.h` 里的 `KERN_*` 级别宏。
//!
//! 保留原版的三件核心行为：
//! 1. 消息前缀 `"<N>"` 指定级别（原版 `KERN_EMERG` … `KERN_DEBUG` 就是这个字符串）
//! 2. 4KB 环形日志缓冲 `log_buf`，供后来的 `sys_syslog` / dmesg 读取
//! 3. 只有级别严于 `console_loglevel` 的消息才送到控制台
//!
//! 没移植的部分：`sys_syslog()` 的读侧（要等系统调用和 `wait_queue`）、
//! `register_console()` 的回放（我们的控制台在 `start_kernel` 第一行就绪，
//! 不存在原版那段「控制台还没初始化就 printk」的窗口）。

use crate::console;
use crate::serial;
use core::fmt::Write;

/// 日志级别，取值同原版 `KERN_*`（数字越小越严重）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    /// `<0>` 系统不可用
    Emerg = 0,
    /// `<1>` 必须立即处理
    Alert = 1,
    /// `<2>` 临界情况
    Crit = 2,
    /// `<3>` 错误
    Err = 3,
    /// `<4>` 警告
    Warning = 4,
    /// `<5>` 正常但值得注意
    Notice = 5,
    /// `<6>` 提示信息
    Info = 6,
    /// `<7>` 调试
    Debug = 7,
}

impl Level {
    /// 从 `"<N>"` 里的数字还原级别，越界按 `Debug`。
    pub const fn from_digit(d: u8) -> Level {
        match d {
            0 => Level::Emerg,
            1 => Level::Alert,
            2 => Level::Crit,
            3 => Level::Err,
            4 => Level::Warning,
            5 => Level::Notice,
            6 => Level::Info,
            _ => Level::Debug,
        }
    }

    /// 级别名，用于给控制台上色和加前缀。原版没有（它只打数字）。
    pub const fn tag(self) -> &'static str {
        match self {
            Level::Emerg => "EMERG",
            Level::Alert => "ALERT",
            Level::Crit => "CRIT",
            Level::Err => "ERROR",
            Level::Warning => "WARN",
            Level::Notice => "NOTICE",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }

    /// 控制台配色：越严重越刺眼。原版是纯文本，这里借 VGA 的 16 色区分一下。
    const fn color(self) -> console::Color {
        match self {
            Level::Emerg | Level::Alert | Level::Crit => console::Color::LightRed,
            Level::Err => console::Color::Red,
            Level::Warning => console::Color::Yellow,
            Level::Notice => console::Color::White,
            Level::Info => console::Color::LightGray,
            Level::Debug => console::Color::DarkGray,
        }
    }
}

/// 原版 `LOG_BUF_LEN`。
const LOG_BUF_LEN: usize = 4096;
/// 原版 `DEFAULT_CONSOLE_LOGLEVEL`。
const DEFAULT_CONSOLE_LOGLEVEL: u8 = 7;
/// 原版 `DEFAULT_MESSAGE_LOGLEVEL`：没写 `<N>` 前缀时的默认级别。
const DEFAULT_MESSAGE_LEVEL: Level = Level::Debug;

/// 环形日志缓冲，对应原版的 `log_buf` / `log_start` / `logged_chars`。
struct LogBuf {
    buf: [u8; LOG_BUF_LEN],
    /// 下一个写入位置
    head: usize,
    /// 累计写入字节数（用于判断是否绕圈、以及给 dmesg 算可读长度）
    total: usize,
}

static mut LOG: LogBuf = LogBuf { buf: [0; LOG_BUF_LEN], head: 0, total: 0 };

/// 原版全局 `console_loglevel`：只有级别数字 **小于** 它的消息才上控制台。
static mut CONSOLE_LOGLEVEL: u8 = DEFAULT_CONSOLE_LOGLEVEL;

/// 是否把每条日志也镜像到串口。原版 1.0.9 没有串口控制台
/// （printk.c 的注释里写着 "in preparation for a serial line console (someday)"），
/// 我们的测试脚本靠串口抓日志，所以默认开。
static mut SERIAL_ECHO: bool = true;

/// 取环形缓冲的独占引用。
///
/// # Safety
/// 当前是单核、无抢占；调用方必须保证不与中断上下文的 printk 并发
/// （原版靠 `cli()`/`restore_flags()`，等中断模块到位后在这里补上）。
unsafe fn log() -> &'static mut LogBuf {
    // SAFETY: 契约由调用方保证；addr_of_mut 避免直接借用 static mut。
    unsafe { &mut *core::ptr::addr_of_mut!(LOG) }
}

/// 设置控制台日志级别，返回旧值。对应原版 `sys_syslog(8, ...)`。
pub fn set_console_loglevel(level: u8) -> u8 {
    let p = core::ptr::addr_of_mut!(CONSOLE_LOGLEVEL);
    // SAFETY: 单核无抢占下对一个 u8 的读改写；只有 printk 路径会读它。
    unsafe {
        let old = *p;
        *p = level;
        old
    }
}

/// 当前控制台日志级别。
pub fn console_loglevel() -> u8 {
    // SAFETY: 只读一个 u8。
    unsafe { *core::ptr::addr_of!(CONSOLE_LOGLEVEL) }
}

/// 打开/关闭串口镜像。
pub fn set_serial_echo(on: bool) {
    // SAFETY: 单核无抢占下写一个 bool。
    unsafe { *core::ptr::addr_of_mut!(SERIAL_ECHO) = on }
}

/// 已写入日志缓冲的总字节数（含被覆盖的）。
pub fn logged_chars() -> usize {
    // SAFETY: 只读。
    unsafe { (*core::ptr::addr_of!(LOG)).total }
}

/// 缓冲里当前还留着的日志字节数，最多 [`LOG_BUF_LEN`]。
pub fn log_size() -> usize {
    logged_chars().min(LOG_BUF_LEN)
}

/// 把缓冲里现存的日志按时间顺序拷进 `out`，返回拷贝的字节数。
/// 对应原版 `sys_syslog` 的 type 3（读最后 4K）。
pub fn read_log(out: &mut [u8]) -> usize {
    // SAFETY: 单核无抢占，读期间不会有并发写（调用方应在中断关闭时调用）。
    let lb = unsafe { log() };
    let size = lb.total.min(LOG_BUF_LEN);
    // 绕过一圈后，最老的一条紧跟在 head 后面
    let start = if lb.total > LOG_BUF_LEN { lb.head } else { 0 };
    let n = size.min(out.len());
    // 只取最后 n 字节（out 装不下时丢弃最老的部分，同原版行为）
    let skip = size - n;
    for i in 0..n {
        out[i] = lb.buf[(start + skip + i) % LOG_BUF_LEN];
    }
    n
}

/// `printk!` 的实际实现，不要直接调用。
///
/// 完整对应原版 `printk()` 的主体：解析级别前缀 → 写环形缓冲 →
/// 按 `console_loglevel` 决定是否上控制台。
pub fn _printk(args: core::fmt::Arguments) {
    // 原版是先 vsprintf 到一个 1KB 的 static buf 再逐字符处理，这里同构：
    // 需要先落到缓冲才能解析开头的 "<N>" 前缀。
    let mut buf = [0u8; 1024];
    let n = crate::klib::vsprintf::vsprintf(&mut buf, args);
    let n = n.min(buf.len());
    let msg = &buf[..n];

    // 解析 "<N>" 级别前缀（原版 printk 里那段 `if (msg[0]=='<' ...)`）
    let (level, body) = if msg.len() >= 3 && msg[0] == b'<' && msg[2] == b'>' && msg[1].is_ascii_digit() {
        (Level::from_digit(msg[1] - b'0'), &msg[3..])
    } else {
        (DEFAULT_MESSAGE_LEVEL, msg)
    };

    emit(level, body);
}

/// 已知级别时的快路径，跳过 `"<N>"` 前缀解析。
pub fn _printk_level(level: Level, args: core::fmt::Arguments) {
    let mut buf = [0u8; 1024];
    let n = crate::klib::vsprintf::vsprintf(&mut buf, args);
    let n = n.min(buf.len());
    emit(level, &buf[..n]);
}

/// 写环形缓冲 + 按级别决定去向。
fn emit(level: Level, body: &[u8]) {
    // 1. 无条件进环形缓冲（原版：不管 loglevel，log_buf 都记）
    // SAFETY: 单核无抢占；调用方不应在中断里与非中断路径并发 printk。
    let lb = unsafe { log() };
    for &b in body {
        lb.buf[lb.head] = b;
        lb.head = (lb.head + 1) % LOG_BUF_LEN;
        lb.total += 1;
    }

    // 2. 级别不够严重就不上控制台（原版：if (msg_level < console_loglevel)）
    if (level as u8) >= console_loglevel() {
        return;
    }

    // body 是 vsprintf 的产物，来源是 Rust 的 format_args，一定是有效 UTF-8；
    // 但截断可能切断多字节序列，所以用 lossy 的判断方式而不是 unwrap。
    let text = core::str::from_utf8(body).unwrap_or("<non-utf8 log>");

    console::_print_colored(level.color(), console::Color::Black, format_args!("{}", text));

    // SAFETY: 只读一个 bool。
    if unsafe { *core::ptr::addr_of!(SERIAL_ECHO) } {
        let _ = write!(serial::Writer, "[{}] {}", level.tag(), text);
    }
}

/// 内核日志输出，用法同 `println!`，但会经过环形缓冲和 loglevel 过滤。
/// 支持原版的 `"<N>"` 前缀写法：`printk!("<3>oops: {}\n", code)`。
///
/// 注意：和原版一样**不**自动补换行，需要自己写 `\n`。
#[macro_export]
macro_rules! printk {
    ($($arg:tt)*) => ($crate::klib::printk::_printk(format_args!($($arg)*)));
}

/// 按级别输出并自动补换行。`pr_info!("mem: {}k", n)` 之类。
#[macro_export]
macro_rules! pr {
    ($level:expr, $($arg:tt)*) => (
        $crate::klib::printk::_printk_level($level, format_args!("{}\n", format_args!($($arg)*)))
    );
}

/// `KERN_EMERG` 级日志
#[macro_export]
macro_rules! pr_emerg {
    ($($arg:tt)*) => ($crate::pr!($crate::klib::printk::Level::Emerg, $($arg)*));
}

/// `KERN_ALERT` 级日志
#[macro_export]
macro_rules! pr_alert {
    ($($arg:tt)*) => ($crate::pr!($crate::klib::printk::Level::Alert, $($arg)*));
}

/// `KERN_CRIT` 级日志
#[macro_export]
macro_rules! pr_crit {
    ($($arg:tt)*) => ($crate::pr!($crate::klib::printk::Level::Crit, $($arg)*));
}

/// `KERN_ERR` 级日志
#[macro_export]
macro_rules! pr_err {
    ($($arg:tt)*) => ($crate::pr!($crate::klib::printk::Level::Err, $($arg)*));
}

/// `KERN_WARNING` 级日志
#[macro_export]
macro_rules! pr_warn {
    ($($arg:tt)*) => ($crate::pr!($crate::klib::printk::Level::Warning, $($arg)*));
}

/// `KERN_NOTICE` 级日志
#[macro_export]
macro_rules! pr_notice {
    ($($arg:tt)*) => ($crate::pr!($crate::klib::printk::Level::Notice, $($arg)*));
}

/// `KERN_INFO` 级日志
#[macro_export]
macro_rules! pr_info {
    ($($arg:tt)*) => ($crate::pr!($crate::klib::printk::Level::Info, $($arg)*));
}

/// `KERN_DEBUG` 级日志
#[macro_export]
macro_rules! pr_debug {
    ($($arg:tt)*) => ($crate::pr!($crate::klib::printk::Level::Debug, $($arg)*));
}
