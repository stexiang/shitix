//! 键盘驱动。对应 linux-1.0.9 的 `drivers/char/keyboard.c`。
//!
//! # 移植范围
//!
//! 原版 `keyboard.c` 有 1200 行 + 生成的 `defkeymap.c`：
//! 完整的 keymap（4 张 256 项表：普通/Shift/AltGr/Ctrl）、
//! 死键与重音组合（`diacr.h`）、小键盘与 NumLock、功能键送转义序列、
//! LED 控制（`kbd_leds` 走 0x60 命令）、控制台切换热键
//! （Alt+F1..F12 调 `change_console`）、`SysRq`、
//! Ctrl+Alt+Del 触发 `ctrl_alt_del()`，以及 e0 扩展扫描码的多字节状态机。
//!
//! 这里只做「够跑一个 shell」的那部分：
//! - Set 1 扫描码的主键区（字母、数字、符号、空格、回车、退格、Tab、Esc）
//! - Shift / Ctrl / CapsLock 修饰
//! - Ctrl+字母 → 控制字符（这条是必需的：^C/^D/^U 是 tty 行规则的输入）
//! - `0xE0` 前缀吃掉（方向键等扩展键忽略，不送半截字节给 tty）
//!
//! 不做：AltGr、死键、小键盘、功能键、LED、控制台切换、Ctrl+Alt+Del。
//!
//! 键盘控制器端口同原版 `include/asm/io.h` 用法：
//! 0x60 是数据口，0x61 是老 XT 的应答口（PC/AT 之后不需要，但原版
//! `keyboard.S` 里那段 `inb 0x61; orb 0x80; outb` 应答仍然无害），
//! 0x64 是状态口。

use crate::irq;
use crate::pr_info;

/// 键盘数据口。原版 `KBD_DATA_REG 0x60`。
const KBD_DATA: u16 = 0x60;
/// 键盘状态口。原版 `KBD_STATUS_REG 0x64`。
const KBD_STATUS: u16 = 0x64;
/// 状态口的「输出缓冲满」位。原版 `KBD_STAT_OBF 0x01`。
const KBD_STAT_OBF: u8 = 0x01;

/// 键盘中断号。IRQ1，同原版 `request_irq(KEYBOARD_IRQ, keyboard_interrupt, …)`。
const KEYBOARD_IRQ: usize = 1;

/// 修饰键状态。原版是 `shift_state` 位图 + `kbd->lockstate`。
static mut SHIFT: bool = false;
static mut CTRL: bool = false;
static mut ALT: bool = false;
static mut CAPS: bool = false;
/// 上一个字节是 0xE0（扩展扫描码前缀）。原版 `e0_keys` 那套状态机。
static mut E0_PREFIX: bool = false;

/// 已处理的按键数。原版没有；自检用。
static mut KEY_COUNT: u64 = 0;

/// 未修饰的扫描码 → ASCII。Set 1 扫描码，下标就是扫描码。
/// 对应原版 `defkeymap.c` 的 `plain_map[]`（原版存的是 `K(KT_LATIN,ch)`
/// 编码过的 u16，我们直接存字节，0 表示无映射）。
static PLAIN_MAP: [u8; 0x40] = [
    0, 0x1b, b'1', b'2', b'3', b'4', b'5', b'6', // 00-07
    b'7', b'8', b'9', b'0', b'-', b'=', 0x08, b'\t', // 08-0f
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', // 10-17
    b'o', b'p', b'[', b']', b'\n', 0, b'a', b's', // 18-1f (1d = LCtrl)
    b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', // 20-27
    b'\'', b'`', 0, b'\\', b'z', b'x', b'c', b'v', // 28-2f (2a = LShift)
    b'b', b'n', b'm', b',', b'.', b'/', 0, b'*', // 30-37 (36 = RShift)
    0, b' ', 0, 0, 0, 0, 0, 0, // 38-3f (38 = LAlt, 3a = CapsLock)
];

/// Shift 按下时的映射。对应原版 `shift_map[]`。
static SHIFT_MAP: [u8; 0x40] = [
    0, 0x1b, b'!', b'@', b'#', b'$', b'%', b'^', // 00-07
    b'&', b'*', b'(', b')', b'_', b'+', 0x08, b'\t', // 08-0f
    b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', // 10-17
    b'O', b'P', b'{', b'}', b'\n', 0, b'A', b'S', // 18-1f
    b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':', // 20-27
    b'"', b'~', 0, b'|', b'Z', b'X', b'C', b'V', // 28-2f
    b'B', b'N', b'M', b'<', b'>', b'?', 0, b'*', // 30-37
    0, b' ', 0, 0, 0, 0, 0, 0, // 38-3f
];

/// 扫描码常量。对应原版 keyboard.c 里那些魔数。
mod sc {
    pub const LSHIFT: u8 = 0x2a;
    pub const RSHIFT: u8 = 0x36;
    pub const CTRL: u8 = 0x1d;
    pub const ALT: u8 = 0x38;
    pub const CAPSLOCK: u8 = 0x3a;
    /// 扩展扫描码前缀
    pub const E0: u8 = 0xe0;
}

/// 从端口读一个字节。
///
/// # Safety
/// `port` 必须是合法的 I/O 端口。这里只用于 0x60/0x64。
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: 契约保证端口合法。不加 nomem/nostack：`in` 本身不触发异常，
    // 但保守起见与项目里其他端口访问保持一致的写法（见 cerebrum 里
    // 关于 asm! 选项的那条）。
    unsafe {
        core::arch::asm!("in al, dx", out("al") v, in("dx") port);
    }
    v
}

/// 键盘中断处理。对应原版 `keyboard_interrupt()`（在 `keyboard.S` 里，
/// 由 `handle_scancode()` 做实际转换）。
///
/// # Safety
/// 由 `irq::do_IRQ` 在中断上下文调用。只碰端口、本模块的静态量和
/// tty 的 `read_q`（`receive_char` 保证不睡）。
fn keyboard_interrupt(_irq: usize, _regs: &mut crate::traps::PtRegs) {
    // SAFETY: 中断上下文；下面所有操作都不睡也不重入。
    unsafe {
        // 原版先查状态口确认真有数据（避免读到陈旧值）
        if inb(KBD_STATUS) & KBD_STAT_OBF == 0 {
            return;
        }
        let code = inb(KBD_DATA);

        if code == sc::E0 {
            *core::ptr::addr_of_mut!(E0_PREFIX) = true;
            return;
        }
        if *core::ptr::addr_of!(E0_PREFIX) {
            // 扩展键（方向键、右 Ctrl/Alt、Home/End…）：吃掉不处理，
            // 见模块文档。原版在这里查 e0 专用映射。
            *core::ptr::addr_of_mut!(E0_PREFIX) = false;
            return;
        }

        // 最高位是「松开」标志（原版 `if (scancode & 0x80) up_flag = 1`）
        let released = code & 0x80 != 0;
        let sc_code = code & 0x7f;

        match sc_code {
            sc::LSHIFT | sc::RSHIFT => {
                *core::ptr::addr_of_mut!(SHIFT) = !released;
                return;
            }
            sc::CTRL => {
                *core::ptr::addr_of_mut!(CTRL) = !released;
                return;
            }
            sc::ALT => {
                *core::ptr::addr_of_mut!(ALT) = !released;
                return;
            }
            sc::CAPSLOCK => {
                // 原版只在按下时翻转（`if (!up_flag) chg_vc_kbd_led(...)`）
                if !released {
                    let c = &mut *core::ptr::addr_of_mut!(CAPS);
                    *c = !*c;
                }
                return;
            }
            _ => {}
        }

        // 松开普通键：无事可做（原版只对修饰键和自动重复关心 up_flag）
        if released {
            return;
        }
        let idx = sc_code as usize;
        if idx >= PLAIN_MAP.len() {
            return;
        }

        let shift = *core::ptr::addr_of!(SHIFT);
        let caps = *core::ptr::addr_of!(CAPS);
        let ctrl = *core::ptr::addr_of!(CTRL);

        let mut ch = if shift { SHIFT_MAP[idx] } else { PLAIN_MAP[idx] };
        if ch == 0 {
            return;
        }
        // CapsLock 只影响字母（原版同样只对 KT_LETTER 生效）
        if caps && ch.is_ascii_alphabetic() {
            ch = if shift { ch.to_ascii_lowercase() } else { ch.to_ascii_uppercase() };
        }
        // Ctrl+字母 → 控制字符。原版 `if (ctrl) value &= 0x1f`。
        // 这一步是 tty 行规则的输入前提（^C/^D/^U 都从这里来）。
        if ctrl {
            if ch.is_ascii_alphabetic() {
                ch = ch.to_ascii_uppercase() & 0x1f;
            } else {
                match ch {
                    b'[' => ch = 0x1b,
                    b'\\' => ch = 0x1c,
                    b']' => ch = 0x1d,
                    b'?' => ch = 0o177,
                    _ => {}
                }
            }
        }

        *core::ptr::addr_of_mut!(KEY_COUNT) += 1;
        super::tty::receive_char(ch);
    }
}

/// 注册键盘中断。对应原版 `kbd_init()` 里的
/// `request_irq(KEYBOARD_IRQ, keyboard_interrupt, 0, "keyboard")`。
///
/// # Safety
/// 启动期调用一次，此时 PIC 与 IDT 已就绪、tty 已初始化。
pub unsafe fn init() {
    // SAFETY: 契约转交。
    unsafe {
        // 原版把键盘装成 fast interrupt（不做 bottom half 之前的完整
        // SAVE_ALL），我们用普通中断：`receive_char` 很短，
        // 而 fast 路径在我们的 entry.S 里不允许调 Rust 函数以外的东西。
        if let Err(e) = irq::request_irq(KEYBOARD_IRQ, keyboard_interrupt, false) {
            pr_info!("keyboard: request_irq failed: {}", crate::klib::errno::strerror(e));
            return;
        }
        // 先把控制器里可能残留的一个字节读掉，否则 OBF 一直置位、
        // 中断进不来（原版靠 `kbd_init` 末尾那次 `inb(0x60)` 达到同样目的）
        if inb(KBD_STATUS) & KBD_STAT_OBF != 0 {
            let _ = inb(KBD_DATA);
        }
        irq::enable_irq(KEYBOARD_IRQ);
    }
    pr_info!("keyboard: irq {} enabled (US layout, main block only)", KEYBOARD_IRQ);
}

/// 已处理的按键数。自检用。
pub fn key_count() -> u64 {
    // SAFETY: 中断里自增、这里只读，必须 volatile——否则等待循环会被
    // 提升成死循环（见 cerebrum 里 jiffies 那条）。
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(KEY_COUNT)) }
}

/// 修饰键状态。自检与调试用。
pub fn modifiers() -> (bool, bool, bool, bool) {
    // SAFETY: 只读四个 bool；单次读取的撕裂不影响调试输出。
    unsafe {
        (
            *core::ptr::addr_of!(SHIFT),
            *core::ptr::addr_of!(CTRL),
            *core::ptr::addr_of!(ALT),
            *core::ptr::addr_of!(CAPS),
        )
    }
}
