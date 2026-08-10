//! tty 层。对应 linux-1.0.9 的 `drivers/char/tty_io.c` +
//! `include/linux/tty.h` 的 `struct tty_queue`/`struct tty_struct`，
//! 以及 `include/linux/termios.h`。
//!
//! # 与原版的结构性差异
//!
//! 1. **三队列保留，`read_q`/`write_q`/`secondary` 语义不变**：
//!    - `read_q`：中断（键盘）塞进来的原始字符
//!    - `secondary`：行规则处理过、可以交给 `read()` 的字符
//!    - `write_q`：`write()` 塞进来、等着输出的字符
//!    原版这三个队列都是 `struct tty_queue { head, tail, proc_list, buf[1024] }`
//!    的环形缓冲，我们照搬（[`TtyQueue`]），只把 `proc_list` 从裸指针
//!    等待队列换成 [`WaitQueue`]。
//! 2. **只有一个 tty**。原版支持 `NR_CONSOLES` 个虚拟控制台 + 64 个串口 +
//!    伪终端，`tty_table[]` 按 minor 索引，还有 `redirect`（`/dev/tty`
//!    指向当前进程的控制终端）。我们只有 `/dev/tty0`（也就是 `/dev/console`），
//!    所以 [`TTY`] 是单个静态实例。
//! 3. **行规则内联在 [`copy_to_cooked`] 里**。原版也是这样
//!    （1.0.9 还没有 N_TTY 这层可插拔的 line discipline 抽象，
//!    `tty->disc` 字段存在但只有 `copy_to_cooked` 一种实现）。
//! 4. **`termios` 里只实现真正会被检查的那些位**：`ICANON`/`ECHO`/`ISIG`/
//!    `ICRNL`/`OPOST`/`ONLCR`。其余（`IXON` 软流控、`XCASE`、`ECHOPRT`
//!    之类）字段保留在结构体里以便 `tcgetattr` 返回得对，但不参与判断，
//!    并在字段上注明。
//! 5. **不移植 `tty_ioctl.c` 的全部 ioctl**：只做 `TCGETS`/`TCSETS`
//!    （`tcgetattr`/`tcsetattr` 的内核侧）和 `TIOCGWINSZ`。
//!    原版还有波特率设置、流控、`TIOCSTI` 注入、包模式等等，
//!    都依赖串口驱动或伪终端。

use crate::sched::WaitQueue;
use crate::{pr_info, pr_warn};

/// 队列容量。对应原版 `tty.h` 的 `TTY_BUF_SIZE 1024`。
pub const TTY_BUF_SIZE: usize = 1024;

/// 控制字符个数。对应原版 `termios.h` 的 `NCCS 19`。
pub const NCCS: usize = 19;

/// `c_cc` 的下标。对应原版 `termios.h` 的 `V*`。
pub mod cc {
    pub const VINTR: usize = 0;
    pub const VQUIT: usize = 1;
    pub const VERASE: usize = 2;
    pub const VKILL: usize = 3;
    pub const VEOF: usize = 4;
    pub const VTIME: usize = 5;
    pub const VMIN: usize = 6;
    pub const VSWTC: usize = 7;
    pub const VSTART: usize = 8;
    pub const VSTOP: usize = 9;
    pub const VSUSP: usize = 10;
    pub const VEOL: usize = 11;
    pub const VREPRINT: usize = 12;
    pub const VDISCARD: usize = 13;
    pub const VWERASE: usize = 14;
    pub const VLNEXT: usize = 15;
    pub const VEOL2: usize = 16;
}

/// `c_iflag` 位。对应原版 `termios.h`（八进制值照抄）。
pub mod iflag {
    pub const IGNBRK: u32 = 0o000001;
    pub const BRKINT: u32 = 0o000002;
    pub const ISTRIP: u32 = 0o000040;
    pub const INLCR: u32 = 0o000100;
    pub const IGNCR: u32 = 0o000200;
    pub const ICRNL: u32 = 0o000400;
    /// 软流控。结构体里有，但我们不实现（见模块文档第 4 点）
    pub const IXON: u32 = 0o002000;
}

/// `c_oflag` 位。
pub mod oflag {
    pub const OPOST: u32 = 0o000001;
    pub const ONLCR: u32 = 0o000004;
    pub const OCRNL: u32 = 0o000010;
}

/// `c_lflag` 位。
pub mod lflag {
    pub const ISIG: u32 = 0o000001;
    pub const ICANON: u32 = 0o000002;
    pub const ECHO: u32 = 0o000010;
    pub const ECHOE: u32 = 0o000020;
    pub const ECHOK: u32 = 0o000040;
    pub const ECHONL: u32 = 0o000100;
    pub const NOFLSH: u32 = 0o000200;
}

/// 终端属性。对应原版 `struct termios`。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Termios {
    /// 输入模式。原版 `tcflag_t c_iflag`
    pub c_iflag: u32,
    /// 输出模式。原版 `c_oflag`
    pub c_oflag: u32,
    /// 控制模式（波特率等）。原版 `c_cflag`。**我们不解释这个字段**：
    /// 唯一的 tty 是 VGA 控制台，没有波特率可言。保留是为了让
    /// `tcgetattr`/`tcsetattr` 能原样往返。
    pub c_cflag: u32,
    /// 本地模式。原版 `c_lflag`
    pub c_lflag: u32,
    /// 行规则编号。原版 `cc_t c_line`；同上，保留但不解释。
    pub c_line: u8,
    /// 控制字符。原版 `cc_t c_cc[NCCS]`
    pub c_cc: [u8; NCCS],
}

impl Termios {
    /// 默认属性。对应原版 `tty_io.c` 的 `tty_std_termios`：
    /// `ICRNL|IXON`、`OPOST|ONLCR`、`ISIG|ICANON|ECHO|ECHOE|ECHOK`。
    pub const fn default_termios() -> Self {
        let mut c_cc = [0u8; NCCS];
        // 原版 INIT_C_CC: "\003\034\177\025\004\0\1\0\021\023\032\0\022\017\027\026\0"
        c_cc[cc::VINTR] = 0o003; // ^C
        c_cc[cc::VQUIT] = 0o034; // ^\
        c_cc[cc::VERASE] = 0o177; // DEL
        c_cc[cc::VKILL] = 0o025; // ^U
        c_cc[cc::VEOF] = 0o004; // ^D
        c_cc[cc::VTIME] = 0;
        c_cc[cc::VMIN] = 1;
        c_cc[cc::VSTART] = 0o021; // ^Q
        c_cc[cc::VSTOP] = 0o023; // ^S
        c_cc[cc::VSUSP] = 0o032; // ^Z
        c_cc[cc::VREPRINT] = 0o022; // ^R
        c_cc[cc::VDISCARD] = 0o017; // ^O
        c_cc[cc::VWERASE] = 0o027; // ^W
        c_cc[cc::VLNEXT] = 0o026; // ^V
        Termios {
            c_iflag: iflag::ICRNL | iflag::IXON,
            c_oflag: oflag::OPOST | oflag::ONLCR,
            // 原版 B38400|CS8|CREAD|HUPCL；我们不解释，填个像样的值
            c_cflag: 0o2277,
            c_lflag: lflag::ISIG | lflag::ICANON | lflag::ECHO | lflag::ECHOE | lflag::ECHOK,
            c_line: 0,
            c_cc,
        }
    }
}

/// 窗口大小。对应原版 `struct winsize`。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// 环形字符队列。对应原版 `struct tty_queue`。
///
/// 原版的 `head`/`tail` 是 `unsigned long` 下标，靠 `INC(a)`
/// （`a = (a+1) & (TTY_BUF_SIZE-1)`）回绕，队满与队空都表现为
/// `head == tail`——原版靠「永远留一个空位」避免混淆
/// （`LEFT(q)` 算的是 `TTY_BUF_SIZE - 1 - 已用`）。照搬。
pub struct TtyQueue {
    /// 写入位置。原版 `head`
    pub head: usize,
    /// 读出位置。原版 `tail`
    pub tail: usize,
    /// 等这个队列的任务。原版 `struct wait_queue * proc_list`
    pub proc_list: WaitQueue,
    /// 环形缓冲。原版 `unsigned char buf[TTY_BUF_SIZE]`
    pub buf: [u8; TTY_BUF_SIZE],
}

impl TtyQueue {
    const fn new() -> Self {
        TtyQueue {
            head: 0,
            tail: 0,
            proc_list: WaitQueue::new(),
            buf: [0; TTY_BUF_SIZE],
        }
    }

    /// 队列里的字符数。对应原版 `CHARS(q)`。
    #[inline]
    pub fn chars(&self) -> usize {
        (self.head.wrapping_sub(self.tail)) & (TTY_BUF_SIZE - 1)
    }

    /// 还能放几个。对应原版 `LEFT(q)`（注意是 `SIZE-1`，见结构体文档）。
    #[inline]
    pub fn left(&self) -> usize {
        TTY_BUF_SIZE - 1 - self.chars()
    }

    /// 对应原版 `EMPTY(q)`。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    /// 对应原版 `FULL(q)`。
    #[inline]
    pub fn is_full(&self) -> bool {
        self.left() == 0
    }

    /// 塞一个字符。对应原版 `put_tty_queue()`。队满则丢弃（同原版）。
    pub fn put(&mut self, c: u8) {
        if self.is_full() {
            return;
        }
        self.buf[self.head] = c;
        self.head = (self.head + 1) & (TTY_BUF_SIZE - 1);
    }

    /// 取一个字符。对应原版 `get_tty_queue()`，队空返回 `None`
    /// （原版返回 -1）。
    pub fn get(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        let c = self.buf[self.tail];
        self.tail = (self.tail + 1) & (TTY_BUF_SIZE - 1);
        Some(c)
    }

    /// 退掉最后一个字符（行编辑的退格用）。对应原版
    /// `DEC(tty->secondary.head)` 那种写法。
    pub fn unput(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        self.head = (self.head.wrapping_sub(1)) & (TTY_BUF_SIZE - 1);
        Some(self.buf[self.head])
    }

    /// 最后一个字符是什么，不取出。原版 `LAST(q)`。
    pub fn last(&self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        Some(self.buf[(self.head.wrapping_sub(1)) & (TTY_BUF_SIZE - 1)])
    }

    /// 清空。对应原版 `flush(q)`。
    pub fn flush(&mut self) {
        self.head = 0;
        self.tail = 0;
    }
}

/// 一个终端。对应原版 `struct tty_struct`。
///
/// 原版有十一个函数指针（`open`/`close`/`write`/`ioctl`/`throttle`/
/// `set_termios`/`stop`/`start`/`hangup`/…）用来对接不同的底层驱动
/// （控制台、串口、伪终端）。我们只有控制台一个底层，所以输出直接调
/// [`super::console::con_write`]，不留 vtable（见 `fs/mod.rs` 文档第 2 点
/// 的同一个理由）。
pub struct Tty {
    /// 终端属性。原版 `struct termios *termios`（原版是指针，指向
    /// `tty_termios[]` 里的一项，因为要在 tty 关闭后保留设置）
    pub termios: Termios,
    /// 前台进程组。原版 `int pgrp`，`ISIG` 时信号送给它
    pub pgrp: i32,
    /// 会话。原版 `int session`
    pub session: i32,
    /// 被 ^S 停住。原版 `stopped:1`
    pub stopped: bool,
    /// 下一个字符按字面处理（^V）。原版 `lnext:1`
    pub lnext: bool,
    /// 打开计数。原版 `int count`
    pub count: i32,
    /// 当前列号，用于 tab 展开与退格。原版 `unsigned int column`
    pub column: u32,
    /// 窗口大小。原版 `struct winsize winsize`
    pub winsize: Winsize,
    /// 键盘中断塞进来的原始字符。原版 `read_q`
    pub read_q: TtyQueue,
    /// 行规则处理完、可供 `read()` 取用的字符。原版 `secondary`
    pub secondary: TtyQueue,
    /// `write()` 塞进来待输出的字符。原版 `write_q`
    pub write_q: TtyQueue,
    /// `secondary` 里已完成的行数（规范模式下 `read` 靠它判断能不能返回）。
    /// 原版没有这个计数器，而是在 `secondary` 上并行维护一个
    /// `secondary_flags[]` 位图标出哪些字节是行尾。我们用计数器：
    /// 位图的唯一用途就是数行尾，而我们不支持 `ECHOPRT` 那种需要
    /// 回溯逐字节标记的重打印。
    pub canon_lines: usize,
}

impl Tty {
    const fn new() -> Self {
        Tty {
            termios: Termios::default_termios(),
            pgrp: 0,
            session: 0,
            stopped: false,
            lnext: false,
            count: 0,
            column: 0,
            winsize: Winsize { ws_row: 25, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 },
            read_q: TtyQueue::new(),
            secondary: TtyQueue::new(),
            write_q: TtyQueue::new(),
            canon_lines: 0,
        }
    }

    /// 规范模式？对应原版 `L_CANON(tty)`。
    #[inline]
    pub fn l_canon(&self) -> bool {
        self.termios.c_lflag & lflag::ICANON != 0
    }
    /// 对应原版 `L_ECHO(tty)`。
    #[inline]
    pub fn l_echo(&self) -> bool {
        self.termios.c_lflag & lflag::ECHO != 0
    }
    /// 对应原版 `L_ISIG(tty)`。
    #[inline]
    pub fn l_isig(&self) -> bool {
        self.termios.c_lflag & lflag::ISIG != 0
    }
    /// 对应原版 `INTR_CHAR(tty)` 一族。
    #[inline]
    fn ch(&self, which: usize) -> u8 {
        self.termios.c_cc[which]
    }
}

/// 唯一的终端。原版是 `struct tty_struct * tty_table[MAX_TTYS]`
/// （见模块文档第 2 点）。
static mut TTY: Tty = Tty::new();

/// 取那个终端。
///
/// # Safety
/// 调用者需保证不与其他 `&mut TTY` 别名同时存在。中断处理
/// （[`super::keyboard`]）与进程上下文都会调，进程侧访问
/// `read_q`/`secondary` 时需要关中断。
#[inline]
pub unsafe fn tty() -> &'static mut Tty {
    // SAFETY: 契约转交；单核内核。
    unsafe { &mut *core::ptr::addr_of_mut!(TTY) }
}

/// 回显一个字符。对应原版 `put_tty_queue(c, &tty->write_q)` + `tty->write()`
/// 那套，但我们直接写控制台（见 [`Tty`] 文档）。
///
/// 原版把控制字符回显成 `^X` 两个字符（`ECHOCTL`），并维护 `column`
/// 以便 tab 与退格正确。这里保留 `^X` 展开和 column 维护。
///
/// # Safety
/// `t` 必须是 [`tty`] 返回的引用。可在中断上下文调用（只写 VGA）。
unsafe fn echo_char(t: &mut Tty, c: u8) {
    // SAFETY: 契约转交；con_write 只碰 VGA 显存与光标端口。
    unsafe {
        match c {
            b'\n' => {
                super::console::con_write(b"\r\n");
                t.column = 0;
            }
            b'\r' => {
                super::console::con_write(b"\r");
                t.column = 0;
            }
            b'\t' => {
                super::console::con_write(b"\t");
                t.column = (t.column + 8) & !7;
            }
            8 | 0o177 => {
                // 退格：原版 ECHOE 下打 "\b \b" 把字符擦掉
                super::console::con_write(b"\x08 \x08");
                t.column = t.column.saturating_sub(1);
            }
            0..=31 => {
                // 控制字符显示成 ^X
                super::console::con_write(&[b'^', c + b'@']);
                t.column += 2;
            }
            _ => {
                super::console::con_write(&[c]);
                t.column += 1;
            }
        }
    }
}

/// 把 `read_q` 里的原始字符加工进 `secondary`。
/// 对应原版 `tty_io.c` 的 `copy_to_cooked()`。
///
/// 原版处理顺序（照搬）：
/// 1. `ISTRIP` 剥高位、`IUCLC` 转小写（我们不实现 IUCLC，见模块文档第 4 点）
/// 2. `\r`：`IGNCR` 丢弃 / `ICRNL` 转 `\n`
/// 3. `\n`：`INLCR` 转 `\r`
/// 4. 规范模式下 `VERASE` 退格、`VKILL` 删整行
/// 5. `ISIG` 下 `VINTR`/`VQUIT`/`VSUSP` 发信号
/// 6. 行尾（`\n`/`VEOL`/`VEOF`）时 `canon_lines += 1` 并唤醒读者
/// 7. `ECHO` 回显
///
/// 原版还处理 `IXON` 软流控（`VSTART`/`VSTOP` 改 `tty->stopped`）和
/// `VLNEXT`（^V 转义下一个字符）；`stopped`/`lnext` 字段留着，^V 也实现了，
/// 但 ^S/^Q 只置位不真的节流输出——我们的输出端是同步的 VGA 写，
/// 没有可节流的队列。
///
/// # Safety
/// 只能在进程上下文调用（`ISIG` 路径会发信号）。调用时需已关中断，
/// 因为要与键盘中断争 `read_q`。
pub unsafe fn copy_to_cooked() {
    // SAFETY: 契约转交。
    unsafe {
        let t = tty();
        while !t.read_q.is_empty() && !t.secondary.is_full() {
            let mut c = match t.read_q.get() {
                Some(c) => c,
                None => break,
            };

            if t.termios.c_iflag & iflag::ISTRIP != 0 {
                c &= 0x7f;
            }

            // ^V：下一个字符按字面走，跳过所有特殊处理
            if t.lnext {
                t.lnext = false;
                t.secondary.put(c);
                if t.l_echo() {
                    echo_char(t, c);
                }
                continue;
            }
            if t.l_canon() && c == t.ch(cc::VLNEXT) && c != 0 {
                t.lnext = true;
                continue;
            }

            if c == b'\r' {
                if t.termios.c_iflag & iflag::IGNCR != 0 {
                    continue;
                }
                if t.termios.c_iflag & iflag::ICRNL != 0 {
                    c = b'\n';
                }
            } else if c == b'\n' && t.termios.c_iflag & iflag::INLCR != 0 {
                c = b'\r';
            }

            if t.l_canon() {
                if c == t.ch(cc::VERASE) && c != 0 {
                    // 退格：从 secondary 里回删一个，但不能跨过行尾
                    if let Some(last) = t.secondary.last() {
                        if last != b'\n' && last != t.ch(cc::VEOF) {
                            t.secondary.unput();
                            if t.l_echo() {
                                echo_char(t, 0o177);
                            }
                        }
                    }
                    continue;
                }
                if c == t.ch(cc::VKILL) && c != 0 {
                    // 删整行
                    while let Some(last) = t.secondary.last() {
                        if last == b'\n' || last == t.ch(cc::VEOF) {
                            break;
                        }
                        t.secondary.unput();
                        if t.l_echo() {
                            echo_char(t, 0o177);
                        }
                    }
                    continue;
                }
            }

            if t.l_isig() {
                // 原版给整个前台进程组发信号。signal.rs 还没移植
                // （见 STATUS.md 的下一阶段），所以这里只记录 + 提示。
                // 一旦 send_sig 就位，这里换成
                // `kill_pg(t.pgrp, SIGINT, true)`。
                if c == t.ch(cc::VINTR) && c != 0 {
                    t.secondary.flush();
                    t.canon_lines = 0;
                    if t.l_echo() {
                        super::console::con_write(b"^C\r\n");
                        t.column = 0;
                    }
                    pending_signal_stub(t.pgrp, SIGINT_STUB);
                    continue;
                }
                if c == t.ch(cc::VQUIT) && c != 0 {
                    t.secondary.flush();
                    t.canon_lines = 0;
                    if t.l_echo() {
                        super::console::con_write(b"^\\\r\n");
                        t.column = 0;
                    }
                    pending_signal_stub(t.pgrp, SIGQUIT_STUB);
                    continue;
                }
            }

            let is_eol = c == b'\n' || (c == t.ch(cc::VEOF) && c != 0) || (c == t.ch(cc::VEOL) && c != 0);

            t.secondary.put(c);
            if is_eol {
                t.canon_lines += 1;
            }
            if t.l_echo() {
                // VEOF（^D）不回显成 ^D，原版同样跳过
                if !(c == t.ch(cc::VEOF) && c != 0 && c != b'\n') {
                    echo_char(t, c);
                }
            }
        }
        // 有完整的行（或非规范模式下有任何字符）就唤醒读者
        let wake = if t.l_canon() { t.canon_lines > 0 } else { !t.secondary.is_empty() };
        if wake {
            t.secondary.proc_list.wake_up_interruptible();
        }
    }
}

/// `SIGINT` 的编号。`src/signal.rs` 到位后从那里引入。
const SIGINT_STUB: i32 = 2;
/// `SIGQUIT` 的编号。同上。
const SIGQUIT_STUB: i32 = 3;

/// 待投递的终端信号。原版直接 `kill_pg(tty->pgrp, sig, 1)`。
///
/// 信号子系统（`kernel/signal.c`）还没移植，此处只记录最后一次请求，
/// 供自检确认 `ISIG` 路径真的走到了。`send_sig` 就位后删掉这个函数，
/// 换成对 `kill_pg` 的调用。
static mut LAST_TTY_SIGNAL: (i32, i32) = (0, 0);

fn pending_signal_stub(pgrp: i32, sig: i32) {
    // SAFETY: 只写一个 (i32,i32)，且只在进程上下文的 copy_to_cooked 里调。
    unsafe { *core::ptr::addr_of_mut!(LAST_TTY_SIGNAL) = (pgrp, sig) }
    pr_warn!("tty: signal {} to pgrp {} dropped (signal.rs not ported yet)", sig, pgrp);
}

/// 最后一次被丢弃的终端信号。自检用。
pub fn last_signal() -> (i32, i32) {
    // SAFETY: 只读两个 i32。
    unsafe { *core::ptr::addr_of!(LAST_TTY_SIGNAL) }
}

// ---- read / write（原版 tty_read / tty_write）----

/// 从 tty 读。对应原版 `tty_io.c` 的 `tty_read()`。
///
/// 规范模式下必须等到一整行才返回（原版靠 `secondary_flags` 里的行尾标记，
/// 我们靠 `canon_lines`，见 [`Tty::canon_lines`]）。非规范模式按
/// `VMIN`/`VTIME` 决定，原版实现得很细（`TIME_CHAR` 还会挂 timer）；
/// 我们只实现 `VMIN` 那半边：有字符就返回，没字符就睡。
///
/// 返回读到的字节数，或负 errno。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。
pub unsafe fn tty_read(buf: &mut [u8]) -> i64 {
    if buf.is_empty() {
        return 0;
    }
    // SAFETY: 契约转交。
    unsafe {
        loop {
            let flags = crate::irq::local_irq_save();
            // 先把键盘攒下的原始字符加工一遍
            copy_to_cooked();
            let t = tty();
            let ready = if t.l_canon() { t.canon_lines > 0 } else { !t.secondary.is_empty() };
            if ready {
                let mut n = 0;
                while n < buf.len() {
                    let c = match t.secondary.get() {
                        Some(c) => c,
                        None => break,
                    };
                    if t.l_canon() {
                        // ^D（VEOF）在行首表示 EOF：不放进缓冲，直接结束本次读
                        if c == t.ch(cc::VEOF) && c != 0 {
                            t.canon_lines -= 1;
                            crate::irq::restore_flags(flags);
                            return n as i64;
                        }
                        buf[n] = c;
                        n += 1;
                        if c == b'\n' {
                            t.canon_lines -= 1;
                            crate::irq::restore_flags(flags);
                            return n as i64;
                        }
                    } else {
                        buf[n] = c;
                        n += 1;
                    }
                }
                crate::irq::restore_flags(flags);
                return n as i64;
            }
            crate::irq::restore_flags(flags);
            // 没数据：睡在 secondary 上等键盘中断唤醒。
            // 原版这里还检查 `current->signal & ~current->blocked` 以便
            // 被信号打断时返回 -ERESTARTSYS；signal.rs 到位后补上。
            (*core::ptr::addr_of_mut!(TTY)).secondary.proc_list.interruptible_sleep_on();
        }
    }
}

/// 往 tty 写。对应原版 `tty_io.c` 的 `tty_write()`。
///
/// 原版把字符塞进 `write_q` 再调 `tty->write(tty)` 让底层驱动异步取走；
/// 我们的底层是同步的 VGA 写，所以 `write_q` 只是过一下
/// （保留它是因为 `OPOST` 处理和 column 维护都以队列为单位，
/// 而且将来接串口 tty 时需要真正的异步队列）。
///
/// `OPOST`/`ONLCR`/`OCRNL` 的输出加工照原版做。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn tty_write(buf: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let t = tty();
        let opost = t.termios.c_oflag & oflag::OPOST != 0;
        let onlcr = t.termios.c_oflag & oflag::ONLCR != 0;
        let ocrnl = t.termios.c_oflag & oflag::OCRNL != 0;

        for &c in buf {
            if opost {
                match c {
                    b'\n' if onlcr => {
                        super::console::con_write(b"\r\n");
                        t.column = 0;
                        continue;
                    }
                    b'\r' if ocrnl => {
                        super::console::con_write(b"\n");
                        t.column = 0;
                        continue;
                    }
                    b'\r' => t.column = 0,
                    b'\t' => t.column = (t.column + 8) & !7,
                    8 => t.column = t.column.saturating_sub(1),
                    0x20..=0x7e => t.column += 1,
                    _ => {}
                }
            }
            super::console::con_write(&[c]);
        }
        buf.len() as i64
    }
}

/// 键盘中断塞一个字符进来。对应原版 `keyboard.c` 里
/// `put_queue()` 往 `tty->read_q` 写那一步。
///
/// # Safety
/// 在中断上下文调用（IRQ1 处理函数里）。只碰 `read_q` 和它的等待队列，
/// 都不会睡。
pub unsafe fn receive_char(c: u8) {
    // SAFETY: 契约转交。
    unsafe {
        let t = tty();
        t.read_q.put(c);
        // 原版这里 `mark_bh(TTY_BH)`，由 bottom half 去跑 copy_to_cooked。
        // 我们的 read 路径每次都会先 copy_to_cooked，所以只需要把睡在
        // secondary 上的读者叫起来让它去加工。
        t.secondary.proc_list.wake_up_interruptible();
    }
}

/// 初始化 tty。对应原版 `tty_init()`。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn init() {
    // Register TTY character device (major=4)
    crate::fs::devices::register_chrdev(
        crate::drivers::block::major::TTY_MAJOR, "tty", crate::fs::devices::CharDev::Tty);
    // SAFETY: 契约保证独占。
    unsafe {
        let t = tty();
        t.termios = Termios::default_termios();
        t.read_q.flush();
        t.secondary.flush();
        t.write_q.flush();
        t.canon_lines = 0;
        t.column = 0;
        t.count = 0;
        t.pgrp = 0;
        t.session = 0;
        t.winsize = Winsize {
            ws_row: crate::console::ROWS as u16,
            ws_col: crate::console::COLS as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
    }
    pr_info!("tty: 1 console, {} byte queues", TTY_BUF_SIZE);
}

/// TCGETS: 将当前 tty 的 termios 复制到用户空间。
///
/// # Safety
/// `arg` 必须指向用户态可写的 `struct termios`（36 字节）。
pub unsafe fn tty_ioctl_get(_fd: usize, arg: usize) -> i64 {
    // SAFETY: single core, process context
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TTY) };
    let src: *const Termios = &t.termios;
    // SAFETY: Termios is 36 bytes, arg is user pointer in identity map
    unsafe { core::ptr::copy_nonoverlapping(src as *const u8, arg as *mut u8, 36); }
    0
}

/// TCSETS: 从用户空间复制 termios 到当前 tty。
///
/// # Safety
/// `arg` 必须指向用户态可读的 `struct termios`（36 字节）。
pub unsafe fn tty_ioctl_set(_fd: usize, arg: usize) -> i64 {
    // SAFETY: single core, process context
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TTY) };
    // SAFETY: copy 36 bytes from user space (identity mapped)
    unsafe { core::ptr::copy_nonoverlapping(arg as *const u8, &mut t.termios as *mut Termios as *mut u8, 36); }
    0
}

/// tty 状态。自检与 `show_state` 用。原版 `tty_io.c` 没有等价物。
pub fn stats() -> (usize, usize, usize, usize) {
    // SAFETY: 关中断读四个 usize，防止键盘中断在中途改 read_q.head。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let t = tty();
        let r = (t.read_q.chars(), t.secondary.chars(), t.canon_lines, t.column as usize);
        crate::irq::restore_flags(flags);
        r
    }
}
