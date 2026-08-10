//! VGA 文本模式控制台（0xB8000, 80x25, 16 色）
//!
//! 对应 linux-1.0.9 的 `kernel/chr_drv/console.c`：原版在 `con_write()` 里
//! 逐字节解析并写 `video_mem_start`，用 `set_cursor()` 通过 CRT 控制器
//! (0x3D4/0x3D5) 同步硬件光标。这里保留同一套机制，但只实现内核早期
//! 输出需要的部分：定位写入、换行、滚屏、颜色属性。
//!
//! 与原版的差异：不做 ESC 序列状态机（原版的 `state`/`par[]`），
//! 颜色通过 [`set_color`] / [`cprint!`] 直接指定。

use core::fmt;

/// VGA 文本缓冲区物理地址。setup.S 建的恒等映射覆盖低 1GB，可直接用物理地址访问。
const VGA_BUF: *mut ScreenChar = 0xB_8000 as *mut ScreenChar;
/// 文本模式列数。给 tty 的 `winsize` 用。
pub const COLS: usize = 80;
/// 文本模式行数。
pub const ROWS: usize = 25;

/// CRT 控制器索引/数据端口，同原版 console.c 的 `video_port_reg/video_port_val`。
const CRT_REG: u16 = 0x3D4;
const CRT_VAL: u16 = 0x3D5;

/// VGA 文本模式的 16 色调色板。判别值即属性字节中的 4 位色号。
#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
#[allow(dead_code)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

/// 属性字节：高 4 位背景色，低 4 位前景色。
#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(transparent)]
pub struct ColorCode(u8);

impl ColorCode {
    pub const fn new(fg: Color, bg: Color) -> Self {
        ColorCode((bg as u8) << 4 | (fg as u8))
    }
}

/// 显存中的一个字符单元，布局必须与硬件一致：低字节字符，高字节属性。
#[derive(Copy, Clone)]
#[repr(C)]
struct ScreenChar {
    ascii: u8,
    color: ColorCode,
}

const DEFAULT_COLOR: ColorCode = ColorCode::new(Color::LightGray, Color::Black);

/// 控制台状态。对应原版 console.c 里的 `x`/`y`/`attr` 三个全局量。
pub struct Writer {
    pub row: usize,
    pub col: usize,
    pub color: ColorCode,
}

/// 全局控制台。内核早期只有一条执行流，不存在并发写入。
static mut WRITER: Writer = Writer { row: 0, col: 0, color: DEFAULT_COLOR };

/// 取全局 `Writer` 的可变引用。
///
/// 用 `addr_of_mut!` 而不是直接借用 `static mut`：edition 2024 禁止后者，
/// 且前者不会先构造一个中间引用。
pub fn writer() -> &'static mut Writer {
    // SAFETY: 内核启动早期为单线程且中断门全部指向 ignore_int（见 head.S），
    // 不存在第二个执行流同时取得这个引用，因此不会出现别名可变引用。
    unsafe { &mut *core::ptr::addr_of_mut!(WRITER) }
}

impl Writer {
    /// 写一个字节。`\n` 换行，`\r` 回到行首，`\t` 对齐到 8 列，其余不可打印字符
    /// 显示为 `.`（原版对未知字节的处理是直接送显存，这里更保守）。
    pub fn putc(&mut self, b: u8) {
        match b {
            b'\n' => self.newline(),
            b'\r' => self.col = 0,
            b'\t' => {
                let next = (self.col + 8) & !7;
                while self.col < next.min(COLS) {
                    self.write_cell(b' ');
                }
            }
            0x08 => {
                // 退格：原版 con_write 的 8 分支，退一格并擦掉字符
                if self.col > 0 {
                    self.col -= 1;
                    let (row, col) = (self.row, self.col);
                    self.put_at(row, col, b' ');
                }
            }
            0x20..=0x7E => self.write_cell(b),
            _ => self.write_cell(b'.'),
        }
    }

    /// 在当前光标处放一个可打印字符并前移光标，必要时折行/滚屏。
    fn write_cell(&mut self, b: u8) {
        if self.col >= COLS {
            self.newline();
        }
        let (row, col) = (self.row, self.col);
        self.put_at(row, col, b);
        self.col += 1;
    }

    /// 直接写显存的唯一出口，集中做边界断言。
    fn put_at(&self, row: usize, col: usize, b: u8) {
        debug_assert!(row < ROWS && col < COLS);
        let cell = ScreenChar { ascii: b, color: self.color };
        // SAFETY: VGA 文本缓冲区固定在物理 0xB8000，被 setup.S 的恒等映射覆盖，
        // 且 row < ROWS、col < COLS 使偏移 < 2000 个单元（4000 字节），不越界。
        // 用 write_volatile 防止编译器合并/消除对 MMIO 的写。
        unsafe { core::ptr::write_volatile(VGA_BUF.add(row * COLS + col), cell) }
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.row + 1 >= ROWS {
            self.scroll();
        } else {
            self.row += 1;
        }
    }

    /// 整屏上移一行，末行以当前属性填空格。对应原版的 `scrup()`。
    fn scroll(&mut self) {
        for row in 1..ROWS {
            for col in 0..COLS {
                // SAFETY: 1 <= row < ROWS 且 col < COLS，源下标 row*COLS+col 与
                // 目标下标 (row-1)*COLS+col 都落在 2000 个单元的缓冲区内。
                unsafe {
                    let v = core::ptr::read_volatile(VGA_BUF.add(row * COLS + col));
                    core::ptr::write_volatile(VGA_BUF.add((row - 1) * COLS + col), v);
                }
            }
        }
        self.row = ROWS - 1;
        for col in 0..COLS {
            self.put_at(ROWS - 1, col, b' ');
        }
        self.col = 0;
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.putc(b);
        }
        Ok(())
    }
}

// ---- 公开接口 ----

/// 清屏并把光标复位到左上角。
pub fn clear() {
    let w = writer();
    w.row = 0;
    w.col = 0;
    for row in 0..ROWS {
        for col in 0..COLS {
            w.put_at(row, col, b' ');
        }
    }
    set_cursor(0, 0);
}

/// 写一个字节并同步硬件光标。给 `drivers/char_dev/console.rs`（tty 的输出端）用。
///
/// 原版对应的是 `console.c:con_write()` 里那个 `switch` 的默认分支
/// （直接把字符送显存）加上末尾的 `set_cursor()`。
pub fn putb(b: u8) {
    writer().putc(b);
    sync_cursor();
}

/// 清除从光标到行尾。
pub fn clear_to_eol() {
    let w = writer();
    let row = w.row;
    for col in w.col..COLS {
        w.put_at(row, col, b' ');
    }
}

/// 清除整行。
pub fn clear_line() {
    let w = writer();
    for col in 0..COLS {
        w.put_at(w.row, col, b' ');
    }
    w.col = 0;
}

/// 设置光标位置（0-based）。
pub fn set_cursor_pos(row: usize, col: usize) {
    let w = writer();
    w.row = row.min(ROWS - 1);
    w.col = col.min(COLS - 1);
    set_cursor(w.row, w.col);
}

/// 获取当前光标行。
pub fn cursor_row() -> usize { writer().row }

/// 获取当前光标列。
pub fn cursor_col() -> usize { writer().col }

/// 清除从光标到屏幕末尾。
pub fn clear_to_end() {
    let w = writer();
    // clear current line from cursor
    for col in w.col..COLS {
        w.put_at(w.row, col, b' ');
    }
    // clear lines below
    for row in (w.row + 1)..ROWS {
        for col in 0..COLS {
            w.put_at(row, col, b' ');
        }
    }
}

/// 设置后续输出的前景/背景色，返回旧属性以便调用方恢复。
pub fn set_color(fg: Color, bg: Color) -> ColorCode {
    let w = writer();
    let old = w.color;
    w.color = ColorCode::new(fg, bg);
    old
}

/// 恢复 [`set_color`] 返回的旧属性。
pub fn restore_color(c: ColorCode) {
    writer().color = c;
}

/// `print!` / `cprint!` 的实际实现，不要直接调用。
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    // 关中断跑完整段。Writer 的 row/col 是非原子的多步更新，CRT 的
    // 索引/数据两次 outb 也必须成对；中断里也会打印（do_timer/do_trap
    // 都会），插进来就会写坏光标位置。原版的 console 输出同样在
    // `cli()`/`restore_flags()` 之间。
    // SAFETY: 只是关中断再恢复；重入由关中断本身排除。
    let flags = unsafe { crate::irq::local_irq_save() };
    // write_str 的实现永不返回 Err，unwrap 会引入 panic 路径，故显式忽略。
    let _ = writer().write_fmt(args);
    sync_cursor();
    // SAFETY: flags 来自上面的 local_irq_save。
    unsafe { crate::irq::restore_flags(flags) }
}

/// 以指定颜色输出一段格式化内容，结束后恢复原属性。
pub fn _print_colored(fg: Color, bg: Color, args: fmt::Arguments) {
    let old = set_color(fg, bg);
    _print(args);
    restore_color(old);
}

/// 把硬件光标移到 `Writer` 当前位置。
pub fn sync_cursor() {
    let w = writer();
    set_cursor(w.row, w.col.min(COLS - 1));
}

/// 通过 CRT 控制器写光标位置寄存器（0x0E 高字节 / 0x0F 低字节），
/// 与原版 console.c 的 `set_cursor()` 一致。
fn set_cursor(row: usize, col: usize) {
    let pos = (row * COLS + col) as u16;
    // SAFETY: CPL=0 下访问 0x3D4/0x3D5 合法；内核独占 CRT 控制器，
    // 无其他驱动会在两次 out 之间改动索引寄存器，索引/数据配对不会被打断。
    unsafe {
        outb(CRT_REG, 0x0E);
        outb(CRT_VAL, (pos >> 8) as u8);
        outb(CRT_REG, 0x0F);
        outb(CRT_VAL, (pos & 0xFF) as u8);
    }
}

/// # Safety
/// 调用者必须保证运行在 CPL=0，且该端口写入对当前设备状态是安全的。
unsafe fn outb(port: u16, val: u8) {
    // SAFETY: 由调用者契约保证在内核态；out 指令本身不访问内存、不改标志位。
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val,
                         options(nomem, nostack, preserves_flags))
    }
}

/// 向 VGA 输出格式化内容，用法同 `std` 的 `print!`。
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

/// 带换行的 [`print!`]。
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// 彩色输出：`cprint!(Color::LightRed, Color::Black, "oops {}", code)`。
#[macro_export]
macro_rules! cprint {
    ($fg:expr, $bg:expr, $($arg:tt)*) => (
        $crate::console::_print_colored($fg, $bg, format_args!($($arg)*))
    );
}

/// 带换行的 [`cprint!`]。
#[macro_export]
macro_rules! cprintln {
    ($fg:expr, $bg:expr, $($arg:tt)*) => (
        $crate::cprint!($fg, $bg, "{}\n", format_args!($($arg)*))
    );
}
