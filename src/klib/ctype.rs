//! 字符分类表。对应 linux-1.0.9 的 `lib/ctype.c` + `include/linux/ctype.h`。
//!
//! 原版用一张 257 字节的 `_ctype[]`（首字节是 EOF 那一格，宏里统一写
//! `(_ctype+1)[c]`）。这里保留同一张表和同样的位定义，只是把宏换成
//! `#[inline]` 函数，并去掉原版 `tolower`/`toupper` 里那个全局临时变量
//! `_ctmp`（多次求值 + 非重入，Rust 侧没必要）。

/// 位掩码，取值与原版 `ctype.h` 完全一致。
pub mod mask {
    /// 大写字母
    pub const U: u8 = 0x01;
    /// 小写字母
    pub const L: u8 = 0x02;
    /// 十进制数字
    pub const D: u8 = 0x04;
    /// 控制字符
    pub const C: u8 = 0x08;
    /// 标点
    pub const P: u8 = 0x10;
    /// 空白（空格 / 换行 / 制表）
    pub const S: u8 = 0x20;
    /// 十六进制数字（仅 a-f / A-F，数字走 `D`）
    pub const X: u8 = 0x40;
    /// 硬空格（0x20）
    pub const SP: u8 = 0x80;
}

use mask::*;

/// 原版 `_ctype[]`，但去掉了开头那个 EOF 格：直接以字符值为下标。
static CTYPE: [u8; 256] = [
    C, C, C, C, C, C, C, C, // 0-7
    C, C | S, C | S, C | S, C | S, C | S, C, C, // 8-15
    C, C, C, C, C, C, C, C, // 16-23
    C, C, C, C, C, C, C, C, // 24-31
    S | SP, P, P, P, P, P, P, P, // 32-39
    P, P, P, P, P, P, P, P, // 40-47
    D, D, D, D, D, D, D, D, // 48-55
    D, D, P, P, P, P, P, P, // 56-63
    P, U | X, U | X, U | X, U | X, U | X, U | X, U, // 64-71
    U, U, U, U, U, U, U, U, // 72-79
    U, U, U, U, U, U, U, U, // 80-87
    U, U, U, P, P, P, P, P, // 88-95
    P, L | X, L | X, L | X, L | X, L | X, L | X, L, // 96-103
    L, L, L, L, L, L, L, L, // 104-111
    L, L, L, L, L, L, L, L, // 112-119
    L, L, L, P, P, P, P, C, // 120-127
    // 128-255：原版全 0（未设置 locale）
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
];

/// 取某个字符的分类位，对应原版的 `(_ctype+1)[c]`。
#[inline]
pub const fn ctype(c: u8) -> u8 {
    CTYPE[c as usize]
}

macro_rules! classifier {
    ($(#[$doc:meta] $name:ident => $bits:expr;)*) => {
        $(
            #[$doc]
            #[inline]
            pub const fn $name(c: u8) -> bool {
                CTYPE[c as usize] & ($bits) != 0
            }
        )*
    };
}

classifier! {
    /// 字母或数字
    isalnum => U | L | D;
    /// 字母
    isalpha => U | L;
    /// 控制字符
    iscntrl => C;
    /// 十进制数字
    isdigit => D;
    /// 可见字符（不含空格）
    isgraph => P | U | L | D;
    /// 小写字母
    islower => L;
    /// 可打印字符（含空格）
    isprint => P | U | L | D | SP;
    /// 标点
    ispunct => P;
    /// 空白
    isspace => S;
    /// 大写字母
    isupper => U;
    /// 十六进制数字
    isxdigit => D | X;
}

/// 是否 7 位 ASCII。对应原版 `isascii()`。
#[inline]
pub const fn isascii(c: u8) -> bool {
    c <= 0x7F
}

/// 截断到 7 位。对应原版 `toascii()`。
#[inline]
pub const fn toascii(c: u8) -> u8 {
    c & 0x7F
}

/// 转小写，非大写字母原样返回。对应原版 `tolower()`。
#[inline]
pub const fn tolower(c: u8) -> u8 {
    if isupper(c) { c + (b'a' - b'A') } else { c }
}

/// 转大写，非小写字母原样返回。对应原版 `toupper()`。
#[inline]
pub const fn toupper(c: u8) -> u8 {
    if islower(c) { c - (b'a' - b'A') } else { c }
}
