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

/// 转义序列解析状态。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EsState {
    Normal,
    Esc,
    Square,
}

static mut ES_STATE: EsState = EsState::Normal;
/// 最多 4 个 CSI 参数（NPAR=4，绝大多数序列用不到更多）
static mut CSI_PAR: [u32; 4] = [0; 4];
static mut CSI_PAR_N: usize = 0;
/// 保存的光标位置
static mut SAVED_ROW: usize = 0;
static mut SAVED_COL: usize = 0;

/// 累加当前参数
unsafe fn csi_add_digit(d: u8) {
    let i = *core::ptr::addr_of!(CSI_PAR_N);
    let p = &mut *core::ptr::addr_of_mut!(CSI_PAR[i]);
    *p = p.wrapping_mul(10).wrapping_add(d as u32);
}

/// 下一个参数
unsafe fn csi_next_param() {
    let n = &mut *core::ptr::addr_of_mut!(CSI_PAR_N);
    if *n < 3 { *n += 1; }
}

/// 取第 i 个参数，默认 def
unsafe fn csi_par(i: usize, def: u32) -> u32 {
    let v = *core::ptr::addr_of!(CSI_PAR[i]);
    // 如果这个槽从未被写过（整个序列没给这个参数），返回默认值
    if *core::ptr::addr_of!(CSI_PAR_N) < i { def } else { if v == 0 && i > 0 { def } else { v } }
}

pub unsafe fn con_write(buf: &[u8]) {
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
                    match c {
                        b'[' => {
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Square;
                            for i in 0..4 { *core::ptr::addr_of_mut!(CSI_PAR[i]) = 0; }
                            *core::ptr::addr_of_mut!(CSI_PAR_N) = 0;
                        }
                        b'c' => {
                            // ESC c: full reset
                            console::clear();
                            console::set_color(Color::LightGray, Color::Black);
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'7' => {
                            // ESC 7: save cursor
                            *core::ptr::addr_of_mut!(SAVED_ROW) = console::cursor_row();
                            *core::ptr::addr_of_mut!(SAVED_COL) = console::cursor_col();
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'8' => {
                            // ESC 8: restore cursor
                            console::set_cursor_pos(
                                *core::ptr::addr_of!(SAVED_ROW),
                                *core::ptr::addr_of!(SAVED_COL));
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        _ => { *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal; }
                    }
                }
                EsState::Square => {
                    match c {
                        b'0'..=b'9' => csi_add_digit(c - b'0'),
                        b';' => csi_next_param(),
                        b'?' => {} // private mode prefix, ignore
                        b'A' => {
                            let n = csi_par(0, 1) as usize;
                            let w = console::writer();
                            w.row = w.row.saturating_sub(n);
                            crate::console::sync_cursor();
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'B' => {
                            let n = csi_par(0, 1) as usize;
                            let w = console::writer();
                            w.row = (w.row + n).min(24);
                            crate::console::sync_cursor();
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'C' => {
                            let n = csi_par(0, 1) as usize;
                            let w = console::writer();
                            w.col = (w.col + n).min(79);
                            crate::console::sync_cursor();
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'D' => {
                            let n = csi_par(0, 1) as usize;
                            let w = console::writer();
                            w.col = w.col.saturating_sub(n);
                            crate::console::sync_cursor();
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'H' | b'f' => {
                            let row = csi_par(0, 1).saturating_sub(1) as usize;
                            let col = csi_par(1, 1).saturating_sub(1) as usize;
                            console::set_cursor_pos(row, col);
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'J' => {
                            match csi_par(0, 0) {
                                0 => console::clear_to_end(),
                                2 => console::clear(),
                                _ => console::clear(),
                            }
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'K' => {
                            match csi_par(0, 0) {
                                0 => console::clear_to_eol(),
                                _ => console::clear_line(),
                            }
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'm' => {
                            let par = csi_par(0, 0);
                            match par {
                                0 => { console::set_color(Color::LightGray, Color::Black); }
                                1 => {} // bold (ignore in VGA text mode)
                                4 => {} // underline (ignore)
                                7 => { console::set_color(Color::Black, Color::LightGray); } // reverse
                                30 => { console::set_color(Color::Black, Color::Black); }     // black fg
                                31 => { console::set_color(Color::Red, Color::Black); }
                                32 => { console::set_color(Color::Green, Color::Black); }
                                33 => { console::set_color(Color::Yellow, Color::Black); }
                                34 => { console::set_color(Color::Blue, Color::Black); }
                                35 => { console::set_color(Color::Magenta, Color::Black); }
                                36 => { console::set_color(Color::Cyan, Color::Black); }
                                37 => { console::set_color(Color::LightGray, Color::Black); } // white
                                40 => {} // black bg (default)
                                41 => { console::set_color(Color::LightGray, Color::Red); }
                                42 => { console::set_color(Color::Black, Color::Green); }
                                43 => { console::set_color(Color::Black, Color::Yellow); }
                                44 => { console::set_color(Color::Black, Color::Blue); }
                                45 => { console::set_color(Color::Black, Color::Magenta); }
                                46 => { console::set_color(Color::Black, Color::Cyan); }
                                47 => { console::set_color(Color::Black, Color::LightGray); } // white bg
                                _ => {}
                            }
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'h' => {
                            // CSI ?25h: show cursor
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
                        b'l' => {
                            // CSI ?25l: hide cursor (ignore)
                            *core::ptr::addr_of_mut!(ES_STATE) = EsState::Normal;
                        }
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

