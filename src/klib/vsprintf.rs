//! 数值/格式化输出。对应 linux-1.0.9 的 `kernel/vsprintf.c`。
//!
//! 原版是 C 可变参数的 `vsprintf(char *buf, const char *fmt, va_list)`。
//! Rust 没有 `va_list`，也没必要——`core::fmt` 已经能做格式化。所以这里
//! 拆成两半，各自对应原版的一个部分：
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`simple_strtoul`] / [`simple_strtol`] | `simple_strtoul()` |
//! | [`skip_atoi`] | `static skip_atoi()` |
//! | [`number`] + [`NumFlags`] | `static number()` |
//! | [`Cursor`] | `vsprintf()` 里那个往 `buf` 里走的 `str` 指针 |
//! | [`vsprintf`] / [`sprintf`] | `vsprintf()` / `sprintf()` |
//!
//! [`vsprintf`] 收的是 `core::fmt::Arguments`（`format_args!` 的产物）而不是
//! `%d` 格式串：内核内部调用点全部改用 Rust 的格式化语法，避免自己实现一遍
//! 类型不安全的 `%` 解析。原版 `number()` 的补位/进制/符号语义则完整保留，
//! 因为 printk 的对齐输出（`%08lx` 之类）还要靠它。

use crate::klib::ctype::{isdigit, isxdigit, tolower};

// ---- 字符串转数字（原版 simple_strtoul）----

/// 解析无符号整数。`base == 0` 时按 C 惯例嗅探前缀：`0x` → 16，`0` → 8，
/// 否则 10。返回 (值, 消耗的字节数)，后者相当于原版的 `*endp - cp`。
///
/// # Safety
/// `cp` 必须指向以 NUL 结尾的可读字节序列。
pub unsafe fn simple_strtoul(cp: *const u8, base: u32) -> (u64, usize) {
    let mut i = 0usize;
    let mut base = base;

    // SAFETY: 调用者保证 cp 可读到 NUL；下面每次 add 前都确认没到 NUL。
    let at = |k: usize| unsafe { *cp.add(k) };

    if base == 0 {
        base = 10;
        if at(i) == b'0' {
            base = 8;
            i += 1;
            if at(i) == b'x' && isxdigit(at(i + 1)) {
                i += 1;
                base = 16;
            }
        }
    }

    let mut result: u64 = 0;
    while isxdigit(at(i)) {
        let c = at(i);
        let value = if isdigit(c) {
            (c - b'0') as u32
        } else {
            (tolower(c) - b'a') as u32 + 10
        };
        if value >= base {
            break;
        }
        result = result.wrapping_mul(base as u64).wrapping_add(value as u64);
        i += 1;
    }
    (result, i)
}

/// 带符号版本。原版没有（1.0.9 只有 `simple_strtoul`），
/// 但解析命令行参数时经常要，补上。
///
/// # Safety
/// 同 [`simple_strtoul`]。
pub unsafe fn simple_strtol(cp: *const u8, base: u32) -> (i64, usize) {
    // SAFETY: 契约保证 cp 可读。
    if unsafe { *cp } == b'-' {
        // SAFETY: 负号后面仍在同一个字符串内。
        let (v, n) = unsafe { simple_strtoul(cp.add(1), base) };
        (-(v as i64), n + 1)
    } else {
        // SAFETY: 契约直接转交。
        let (v, n) = unsafe { simple_strtoul(cp, base) };
        (v as i64, n)
    }
}

/// 从 `s[*i..]` 读一串十进制数字并推进下标。对应原版 `skip_atoi()`。
pub fn skip_atoi(s: &[u8], i: &mut usize) -> usize {
    let mut v = 0usize;
    while *i < s.len() && isdigit(s[*i]) {
        v = v * 10 + (s[*i] - b'0') as usize;
        *i += 1;
    }
    v
}

// ---- number()：整数格式化 ----

/// 原版 `number()` 的 flags 位。取值与 `vsprintf.c` 的宏一致。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NumFlags(pub u32);

impl NumFlags {
    /// 无标志
    pub const NONE: NumFlags = NumFlags(0);
    /// 用 '0' 而不是空格补位
    pub const ZEROPAD: NumFlags = NumFlags(1);
    /// 按有符号数处理
    pub const SIGN: NumFlags = NumFlags(2);
    /// 正数也显示 '+'
    pub const PLUS: NumFlags = NumFlags(4);
    /// 正数前留一个空格
    pub const SPACE: NumFlags = NumFlags(8);
    /// 左对齐
    pub const LEFT: NumFlags = NumFlags(16);
    /// 加进制前缀（0x / 0）
    pub const SPECIAL: NumFlags = NumFlags(32);
    /// 十六进制用小写字母
    pub const SMALL: NumFlags = NumFlags(64);

    /// 是否含某个标志
    #[inline]
    pub const fn has(self, f: NumFlags) -> bool {
        self.0 & f.0 != 0
    }
}

impl core::ops::BitOr for NumFlags {
    type Output = NumFlags;
    fn bitor(self, rhs: NumFlags) -> NumFlags {
        NumFlags(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for NumFlags {
    fn bitor_assign(&mut self, rhs: NumFlags) {
        self.0 |= rhs.0;
    }
}

/// 一个只往固定缓冲区里写的游标，越界就丢弃。
/// 相当于原版 `vsprintf()` 里那个裸的 `char *str`，但不会写飞。
pub struct Cursor<'a> {
    buf: &'a mut [u8],
    /// 已"写"出的字节数（含被截断丢弃的），语义同原版 `str - buf`
    len: usize,
}

impl<'a> Cursor<'a> {
    /// 绑定一块缓冲区。
    pub fn new(buf: &'a mut [u8]) -> Self {
        Cursor { buf, len: 0 }
    }

    /// 写一个字节；缓冲区满则只累加计数。
    #[inline]
    pub fn push(&mut self, b: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = b;
        }
        self.len += 1;
    }

    /// 重复写同一个字节。
    #[inline]
    pub fn push_n(&mut self, b: u8, n: usize) {
        for _ in 0..n {
            self.push(b);
        }
    }

    /// 写一串字节。
    pub fn push_bytes(&mut self, s: &[u8]) {
        for &b in s {
            self.push(b);
        }
    }

    /// 逻辑长度（若发生截断，会大于实际写入量）。
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// 是否什么都没写。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 是否发生了截断。
    #[inline]
    pub fn truncated(&self) -> bool {
        self.len > self.buf.len()
    }

    /// 实际写进缓冲区的部分。
    pub fn written(&self) -> &[u8] {
        &self.buf[..self.len.min(self.buf.len())]
    }

    /// 追加结尾 NUL（不计入 [`len`](Self::len)），让缓冲区能当 C 字符串用。
    /// 缓冲区满时会覆盖最后一个字节，保证一定有终止符。
    pub fn terminate(&mut self) {
        let at = self.len.min(self.buf.len().saturating_sub(1));
        if !self.buf.is_empty() {
            self.buf[at] = 0;
        }
    }
}

impl core::fmt::Write for Cursor<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.push_bytes(s.as_bytes());
        Ok(())
    }
}

/// 最大进制的数字表（原版的 `digits`）。
const DIGITS_UPPER: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGITS_LOWER: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// 按原版 `number()` 的规则格式化一个整数。
///
/// - `size` 是字段宽度，`-1` 表示未指定（原版用 `field_width = -1`）
/// - `precision` 是最少数字位数，`-1` 表示未指定
/// - `base` 超出 2..=36 时什么都不写（原版返回 NULL）
///
/// `num` 取 `i64`：原版在 32 位上 `SIGN` 走 `int`、否则走 `unsigned long`，
/// 这里统一按 `i64` 收，无符号大值请走 [`number_u64`]。
pub fn number(out: &mut Cursor, num: i64, base: u32, size: i32, precision: i32, flags: NumFlags) {
    let (sign, mag) = if flags.has(NumFlags::SIGN) && num < 0 {
        // 用 unsigned_abs 而不是 -num，i64::MIN 也不会溢出
        (Some(b'-'), num.unsigned_abs())
    } else if flags.has(NumFlags::SIGN) {
        let s = if flags.has(NumFlags::PLUS) {
            Some(b'+')
        } else if flags.has(NumFlags::SPACE) {
            Some(b' ')
        } else {
            None
        };
        (s, num as u64)
    } else {
        (None, num as u64)
    };
    number_inner(out, mag, sign, base, size, precision, flags);
}

/// [`number`] 的无符号入口，用于 `%lx` 这类不该被符号扩展的场合。
pub fn number_u64(out: &mut Cursor, num: u64, base: u32, size: i32, precision: i32, flags: NumFlags) {
    // 无符号路径下 SIGN 没有意义，清掉以免误加 '+'
    let flags = NumFlags(flags.0 & !NumFlags::SIGN.0);
    number_inner(out, num, None, base, size, precision, flags);
}

fn number_inner(
    out: &mut Cursor,
    num: u64,
    sign: Option<u8>,
    base: u32,
    size: i32,
    precision: i32,
    mut flags: NumFlags,
) {
    // 原版：if (type&LEFT) type &= ~ZEROPAD;
    if flags.has(NumFlags::LEFT) {
        flags = NumFlags(flags.0 & !NumFlags::ZEROPAD.0);
    }
    if base < 2 || base > 36 {
        return;
    }
    let digits: &[u8; 36] = if flags.has(NumFlags::SMALL) { DIGITS_LOWER } else { DIGITS_UPPER };
    let pad = if flags.has(NumFlags::ZEROPAD) { b'0' } else { b' ' };

    let mut size = size;
    if sign.is_some() {
        size -= 1;
    }
    if flags.has(NumFlags::SPECIAL) {
        match base {
            16 => size -= 2,
            8 => size -= 1,
            _ => {}
        }
    }

    // 逆序生成数字。u64 最长是 base 2 的 64 位
    let mut tmp = [0u8; 64];
    let mut i = 0usize;
    if num == 0 {
        tmp[0] = b'0';
        i = 1;
    } else {
        let mut n = num;
        while n != 0 {
            tmp[i] = digits[(n % base as u64) as usize];
            n /= base as u64;
            i += 1;
        }
    }

    let precision = if (i as i32) > precision { i as i32 } else { precision };
    size -= precision;

    // 右对齐且用空格补位：先补空格。
    // 原版这里是 `while(size-->0) *str++=' ';`，循环退出时 size 已被消耗掉，
    // 后面那段 `if (!(type&LEFT)) while(size-->0)` 就不会再补一遍。这个
    // 副作用必须复现，否则 "%8d" 会补出两倍宽度。
    if !flags.has(NumFlags::ZEROPAD) && !flags.has(NumFlags::LEFT) {
        if size > 0 {
            out.push_n(b' ', size as usize);
        }
        size = 0;
    }
    if let Some(s) = sign {
        out.push(s);
    }
    if flags.has(NumFlags::SPECIAL) {
        match base {
            8 => out.push(b'0'),
            16 => {
                out.push(b'0');
                out.push(digits[33]); // 'X' 或 'x'
            }
            _ => {}
        }
    }
    // 右对齐且用 '0' 补位：补零。同上，原版的 `while(size-->0)` 也会吃掉 size。
    if !flags.has(NumFlags::LEFT) {
        if size > 0 {
            out.push_n(pad, size as usize);
        }
        size = 0;
    }
    // 精度要求的前导零
    if precision > i as i32 {
        out.push_n(b'0', (precision - i as i32) as usize);
    }
    // 数字本体（逆序倒出）
    while i > 0 {
        i -= 1;
        out.push(tmp[i]);
    }
    // 左对齐的尾部空格
    if flags.has(NumFlags::LEFT) && size > 0 {
        out.push_n(b' ', size as usize);
    }
}

// ---- vsprintf / sprintf ----

/// 把 `format_args!` 的结果写进 `buf`，返回逻辑长度（截断时会大于 `buf.len()`）。
/// 对应原版 `vsprintf()`，只是格式化交给 `core::fmt`。
///
/// 不追加结尾 NUL；需要 C 字符串用 [`sprintf`]。
pub fn vsprintf(buf: &mut [u8], args: core::fmt::Arguments) -> usize {
    use core::fmt::Write;
    let mut cur = Cursor::new(buf);
    // Cursor 的 write_str 永不失败，忽略结果以免引入 panic 路径
    let _ = cur.write_fmt(args);
    cur.len()
}

/// 同 [`vsprintf`]，但额外写入结尾 NUL，结果可当 C 字符串用。
/// 对应原版 `sprintf()`。
pub fn sprintf(buf: &mut [u8], args: core::fmt::Arguments) -> usize {
    use core::fmt::Write;
    let mut cur = Cursor::new(buf);
    let _ = cur.write_fmt(args);
    cur.terminate();
    cur.len()
}

/// 往栈上缓冲区格式化，返回 `&str`。用法：`ksprintf!(buf, "{}", x)`。
#[macro_export]
macro_rules! ksprintf {
    ($buf:expr, $($arg:tt)*) => {{
        let n = $crate::klib::vsprintf::vsprintf(&mut $buf[..], format_args!($($arg)*));
        let n = n.min($buf.len());
        core::str::from_utf8(&$buf[..n]).unwrap_or("<non-utf8>")
    }};
}

// =============================================================================
// SAFE WRAPPERS
// =============================================================================

use crate::klib::string::CStr;

/// 解析无符号整数的安全包装器
/// 
/// # Arguments
/// * `s` - 以 NUL 结尾的字节切片
/// * `base` - 进制（0 表示自动检测）
/// 
/// # Returns
/// * `(值, 消耗的字节数)`
pub fn parse_u64(s: &[u8], base: u32) -> Option<(u64, usize)> {
    if s.is_empty() || s[s.len() - 1] != 0 {
        return None; // 不是 NUL 终止
    }
    let s = &s[..s.len() - 1]; // 去掉 NUL
    if s.is_empty() {
        return Some((0, 1));
    }
    
    let mut base = base;
    let mut i = 0usize;
    
    if base == 0 {
        base = 10;
        if s[i] == b'0' {
            base = 8;
            i += 1;
            if i < s.len() && (s[i] == b'x' || s[i] == b'X') {
                if i + 1 < s.len() && isxdigit(s[i + 1]) {
                    i += 1;
                    base = 16;
                } else {
                    return None; // 0x 后没有数字
                }
            }
        }
    }
    
    let mut result: u64 = 0;
    while i < s.len() && isxdigit(s[i]) {
        let c = s[i];
        let value = if isdigit(c) {
            (c - b'0') as u64
        } else {
            (tolower(c) - b'a') as u64 + 10
        };
        if value >= base as u64 {
            break;
        }
        result = result.wrapping_mul(base as u64).wrapping_add(value);
        i += 1;
    }
    
    if i == 0 {
        None
    } else {
        Some((result, i + 1)) // +1 for NUL terminator
    }
}

/// 解析有符号整数的安全包装器
pub fn parse_i64(s: &[u8], base: u32) -> Option<(i64, usize)> {
    if s.is_empty() || s[s.len() - 1] != 0 {
        return None;
    }
    let s = &s[..s.len() - 1];
    if s.is_empty() {
        return Some((0, 1));
    }
    
    let negative = s[0] == b'-';
    let (start, consumed) = if negative {
        (&s[1..], s.len() - 1)
    } else {
        (s, s.len())
    };
    
    if consumed == 0 {
        return None;
    }
    
    match parse_u64(start, base) {
        Some((v, n)) => {
            let result = if negative { -(v as i64) } else { v as i64 };
            Some((result, n + if negative { 1 } else { 0 } + 1)) // +1 for NUL
        }
        None => None,
    }
}

/// 解析 CStr 的无符号整数
pub fn parse_cstr_u64(cstr: &CStr, base: u32) -> Option<(u64, usize)> {
    parse_u64(cstr.as_bytes(), base)
}

/// 解析 CStr 的有符号整数
pub fn parse_cstr_i64(cstr: &CStr, base: u32) -> Option<(i64, usize)> {
    parse_i64(cstr.as_bytes(), base)
}

/// FormatBuf - 安全的格式化缓冲区
pub struct FormatBuf {
    buf: [u8; 256],
    len: usize,
}

impl FormatBuf {
    /// 创建新的格式化缓冲区
    pub fn new() -> Self {
        let mut s = Self {
            buf: [0; 256],
            len: 0,
        };
        s.buf[0] = 0;
        s
    }

    /// 格式化字符串
    pub fn format(&mut self, args: core::fmt::Arguments) {
        use core::fmt::Write;
        // 保留一个字节给 NUL
        let available = &mut self.buf[..255];
        let _ = Cursor::new(available).write_fmt(args);
        self.len = available.len().min(255);
        self.buf[self.len] = 0;
    }

    /// 获取为 CStr
    pub fn as_cstr(&self) -> CStr {
        // SAFETY: 缓冲区总是 NUL 终止
        unsafe { CStr::from_ptr(self.buf.as_ptr()) }
    }

    /// 获取为 str（如果 UTF-8 有效）
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.buf[..self.len]).ok()
    }

    /// 获取长度
    pub fn len(&self) -> usize {
        self.len
    }

    /// 获取字节切片
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Default for FormatBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Write for FormatBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.len + s.len() < 256 {
            self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
            self.buf[self.len] = 0;
        }
        Ok(())
    }
}
