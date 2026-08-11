//! COM1 (0x3F8) 串口输出，供 QEMU `-serial stdio` 抓取，测试脚本据此判定启动结果。

const PORT: u16 = 0x3F8;

/// # Safety
/// 直接写 I/O 端口，仅在 CPL=0 使用。
unsafe fn outb(port: u16, val: u8) {
    // SAFETY: 调用者保证在内核态；out 指令本身不访问内存。
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags)) }
}

/// # Safety
/// 同 outb。
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    // SAFETY: 调用者保证在内核态；in 指令本身不访问内存。
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags))
    }
    val
}

/// 初始化 COM1 为 38400 8N1，关中断、开 FIFO。
pub fn init() {
    // SAFETY: 内核态独占 0x3F8..0x3FF 这组 UART 寄存器，无其他驱动竞争。
    unsafe {
        outb(PORT + 1, 0x00); // 关闭中断
        outb(PORT + 3, 0x80); // 打开 DLAB, 准备设置分频
        outb(PORT + 0, 0x03); // 分频低字节 = 3 -> 38400 baud
        outb(PORT + 1, 0x00); // 分频高字节
        outb(PORT + 3, 0x03); // 8 位数据, 无校验, 1 停止位
        outb(PORT + 2, 0xC7); // 使能 FIFO 并清空
        outb(PORT + 4, 0x0B); // DTR/RTS/OUT2
    }
}

pub fn putc(b: u8) {
    // SAFETY: 内核态轮询 LSR 的 THRE 位后再写数据寄存器，符合 16550 时序。
    unsafe {
        while inb(PORT + 5) & 0x20 == 0 {}
        outb(PORT, b);
    }
}

pub fn print(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            putc(b'\r');
        }
        putc(b);
    }
}

/// 检查串口是否有待接收字符。
pub fn has_char() -> bool {
    unsafe { inb(PORT + 5) & 0x01 != 0 }
}

/// 读 LSR 寄存器（调试用）
pub fn read_lsr() -> u8 {
    unsafe { inb(PORT + 5) }
}

/// 从串口读一个字符（必须先 has_char 确认有数据）。
pub fn getc() -> u8 {
    unsafe { inb(PORT) }
}

/// 让 `write!` / `sprintln!` 能往串口写。
pub struct Writer;

impl core::fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        print(s);
        Ok(())
    }
}

/// `sprint!` / `sprintln!` 的实现，不要直接调用。
pub fn _print(args: core::fmt::Arguments) {
    use core::fmt::Write;
    // write_str 永不返回 Err，忽略结果以免引入 panic 路径
    let _ = Writer.write_fmt(args);
}

/// 格式化输出到串口（COM1）。
#[macro_export]
macro_rules! sprint {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

/// 带换行的 [`sprint!`]。
#[macro_export]
macro_rules! sprintln {
    () => ($crate::sprint!("\n"));
    ($($arg:tt)*) => ($crate::sprint!("{}\n", format_args!($($arg)*)));
}

/// 同时输出到 VGA 和串口。启动期诊断信息用这个，
/// 既能在屏幕上看到，也能被 test.sh 的日志抓到。
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {{
        $crate::println!($($arg)*);
        $crate::sprintln!($($arg)*);
    }};
}

pub fn print_dec(mut v: u64) {
    if v == 0 {
        putc(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut n = 0;
    while v > 0 {
        buf[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    while n > 0 {
        n -= 1;
        putc(buf[n]);
    }
}

/// Print a u64 as hex (no leading 0x).
pub fn raw_hex64(v: u64) {
    putc(b'0');
    putc(b'x');
    let mut empty = true;
    for shift in (0..16).rev() {
        let nibble = ((v >> (shift * 4)) & 0xF) as u8;
        if nibble != 0 || !empty || shift == 0 {
            empty = false;
            putc(if nibble < 10 { b'0' + nibble } else { b'a' + (nibble - 10) });
        }
    }
}

/// Print an i64 in hex with sign.
pub fn raw_hex64_signed(v: i64) {
    if v < 0 {
        putc(b'-');
        raw_hex64((-v) as u64);
    } else {
        raw_hex64(v as u64);
    }
}
