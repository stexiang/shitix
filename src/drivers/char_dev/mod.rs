//! 字符设备。对应 linux-1.0.9 的 `drivers/char/`。
//!
//! # 移植范围
//!
//! | 本模块 | 原版 | 说明 |
//! |---|---|---|
//! | [`tty`] | `tty_io.c` + `tty_ioctl.c` | 三个队列 + 规范模式行编辑 |
//! | [`console`] | `console.c` | tty 的输出端（转发到 `src/console.rs`）|
//! | [`keyboard`] | `keyboard.c` | IRQ1 扫描码 → ASCII |
//! | [`mem`] | `mem.c` | `/dev/null` `/dev/zero` `/dev/full` `/dev/mem` |
//!
//! 原版 `console.c` 有 2000 行：VT102 转义序列状态机（`csi_J`/`csi_K`/
//! `csi_m` 等三十来个）、多虚拟控制台切换、字符集映射、滚动区域、
//! 蜂鸣器。我们的 `src/console.rs`（模块 1）已经实现了滚屏、颜色、
//! 硬件光标和 `\n\r\t\b`，[`console`] 只做 tty 到它的转接。
//! 原版 `keyboard.c` 有完整的 keymap 表（`defkeymap.c` 是生成的）、
//! 组合键、小键盘、LED、控制台切换热键；我们只做 US 布局的
//! 主键区 + Shift/Ctrl/CapsLock，够跑一个 shell 的输入。
//!
//! 不移植：串口 tty（`serial.c` 的 UART 中断驱动 —— 我们的 `src/serial.rs`
//! 是启动期日志用的单向输出端，没有 tty 语义）、伪终端（`pty.c`，需要
//! 成对的 tty 与 `sys_openpty`）、打印机、鼠标、磁带、声卡。

pub mod console;
pub mod keyboard;
pub mod mem;
pub mod tty;

/// 初始化字符设备。对应原版 `chr_dev_init()` 里那串
/// `tty_init(); ... memory_init();`。
///
/// # Safety
/// 启动期调用一次，需在 `fs::devices::init()` 之后（要注册进 chrdevs 表）。
pub unsafe fn init() {
    // SAFETY: 契约转交。顺序同原版：tty 的队列要先建好，
    // keyboard 的中断处理会往里塞字符。
    unsafe {
        tty::init();
        keyboard::init();
        mem::init();
    }
}
