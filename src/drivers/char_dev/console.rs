//! 控制台输出端。对应 linux-1.0.9 的 `drivers/char/console.c`。
//!
//! 原版 `console.c` 有 2000 行，主要是 VT102 终端仿真：
//! `ESC [` 序列的状态机（`ESnormal`/`ESesc`/`ESsquare`/`ESgetpars`/…）、
//! 三十来个 CSI 命令（`csi_J` 清屏、`csi_K` 清行、`csi_m` 设属性、
//! `csi_L`/`csi_M` 插删行）、多虚拟控制台切换（`NR_CONSOLES` 个
//! `vc_data`，每个有独立的显存页和光标状态）、字符集映射（G0/G1、
//! `translations[]` 四张表）、`ESC c` 全复位。
//!
//! 我们的 `src/console.rs`（模块 1）已经做了这套里真正影响可用性的部分：
//! 显存写入、滚屏、`\n\r\t\b`、颜色属性、0x3D4/0x3D5 硬件光标。
//! 本模块因此只是 tty 到它的转接层，加上原版
//! `console.c` 里跟 tty 直接相关的两件事：
//!
//! 1. [`con_write`]：tty 的输出端（原版 `con_write(struct tty_struct*)`
//!    从 `write_q` 取字符送显存）
//! 2. 一个最小的 CSI 子集：只认 `ESC [ 2 J`（清屏）与 `ESC [ m`
//!    （复位属性）。这两个是 shell 提示符和 `clear` 会发的，别的序列
//!    原样丢弃而不是当普通字符打出来——后者会在屏幕上留下 `[0m` 之类的垃圾。
//!
//! 虚拟控制台切换、独立显存、字符集不移植：只有一个控制台。

use crate::console::{self, Color};

/// 转义序列解析状态。对应原版 `console.c` 的 `enum { ESnormal, ESesc,
/// ESsquare, ESgetpars, ESfunckey, … }`，但只保留前三个。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EsState {
    /// 普通字符。原版 `ESnormal`
    Normal,
    /// 刚吃了一个 ESC。原版 `ESesc`
    Esc,
    /// 在 `ESC [` 之后收参数。原版 `ESsquare` + `ESgetpars` 合并
    Square,
}

/// 解析状态。原版存在每个 `vc_data` 里（`vc_state`）。
static mut ES_STATE: EsState = EsState::Normal;

/// CSI 参数缓冲。原版 `par[NPAR]`，NPAR=16；我们只用得到第一个参数
/// （`2J` 的那个 2），但保留累加逻辑以便正确吃掉多参数序列。
static mut CSI_PAR: u32 = 0;

/// 往控制台写一串字符。对应原版 `con_write()`。
///
/// 原版是从 `tty->write_q` 里取字符（签名是 `con_write(struct tty_struct*)`），
/// 我们直接收切片：tty 层的 `write_q` 只是过路（见 `tty.rs` 的
/// [`super::tty::tty_write`] 注释）。
///
/// # Safety
/// 可在中断上下文调用（`printk` 与键盘回显都会调）。只碰 VGA 显存、
/// 光标端口和本模块的三个静态量。
///
/// 注意：这里**没有**关中断保护。原版 `con_write` 同样不关中断
/// （它跑在 bottom half 里，与键盘中断天然不并发）。我们的调用方
/// 是回显（中断上下文）和 `tty_write`（进程上下文），两者会并发，
/// 表现是屏幕上的字符可能交错——与原版在同一台机器上打字同时有输出时
/// 的行为一致，不是内存安全问题。
pub unsafe fn con_write(buf: &[u8]) {
    // SAFETY: 契约转交。
    unsafe {
        for &c in buf {
            let st = *core::ptr::addr_of!(ES_STATE);
            match st {
                EsState::Normal => {
                    if c == 0x1b {
                        *core::ptr::addr_of_mut!(ES_STATE) = EsState::Esc;
                    } else {
                        console::putb(c);
                    }
                }
                EsState::Esc => {
                    if c == b'[' {
                        *core::ptr::addr_of_mut!(ES_STATE) = EsState::Square;
                        *core::ptr::addr_of_mut!(CSI_PAR) = 0;
                    } else {
                        // 原版认 `ESC c`（全复位）、`ESC D`（下移）等等；
                        // 我们只认 CSI，其余丢弃。
                        *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                    }
                }
                EsState::Square => {
                    match c {
                        b'0'..=b'9' => {
                            let p = &mut *core::ptr::addr_of_mut!(CSI_PAR);
                            *p = p.wrapping_mul(10) + (c - b'0') as u32;
                        }
                        b';' => {
                            // 多参数：我们只用第一个，后续参数重新累加即可
                            *core::ptr::addr_of_mut!(CSI_PAR) = 0;
                        }
                        b'J' => {
                            // 原版 csi_J(vc, par[0])：0=清到屏尾 1=清到屏首 2=全清。
                            // 我们的 console 只有整屏清，所以只实现 2。
                            if *core::ptr::addr_of!(CSI_PAR) == 2 {
                                console::clear();
                            }
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'm' => {
                            // 原版 csi_m 支持 0-7 与 30-47 全套；这里只做
                            // `0m`（复位成默认色），别的属性丢弃。
                            if *core::ptr::addr_of!(CSI_PAR) == 0 {
                                console::set_color(Color::LightGray, Color::Black);
                            }
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        // 其余 CSI 终止符（A/B/C/D 光标移动、H 定位、K 清行、
                        // L/M 插删行…）：吃掉但不执行，见模块文档。
                        0x40..=0x7e => {
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
