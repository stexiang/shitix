//! 内核基础库。对应 linux-1.0.9 的 `lib/` 目录 + 被它引用的两个内核文件。
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`ctype`] | `lib/ctype.c` + `include/linux/ctype.h` |
//! | [`string`] | `lib/string.c` + `include/linux/string.h` |
//! | [`errno`] | `lib/errno.c` + `include/linux/errno.h` |
//! | [`vsprintf`] | `kernel/vsprintf.c` |
//! | [`printk`] | `kernel/printk.c` + `include/linux/kernel.h` 的 `KERN_*` |
//!
//! 原版 `lib/` 里剩下的 `_exit.c` / `open.c` / `close.c` / `write.c` /
//! `dup.c` / `setsid.c` / `execve.c` / `wait.c` 是**用户态**系统调用桩
//! （`_syscall0/1/3` 宏展开成 `int $0x80`），给内核里那个 `init()` 直接
//! 调用用。它们要等系统调用入口和进程模型到位才有意义，届时放到
//! `src/syscall/` 而不是这里。`lib/malloc.c` 在 1.0.9 里是空文件。

pub mod ctype;
pub mod errno;
pub mod printk;
pub mod string;
pub mod vsprintf;

pub use errno::KResult;
pub use printk::Level;
pub use vsprintf::{Cursor, NumFlags, number, number_u64, sprintf, vsprintf as format_into};
