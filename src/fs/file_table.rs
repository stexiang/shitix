//! 打开文件表。对应 linux-1.0.9 的 `fs/file_table.c` 与 `fs.h` 的
//! `struct file`。
//!
//! # 与原版的结构性差异
//!
//! 原版的 `file` 也是动态生长的（`grow_files()` 拿一页切开），用
//! `first_file` 双向环串起来，`get_empty_filp` 沿环找 `f_count == 0` 的。
//! 我们固定 [`NR_FILE`] 项、线性扫描 —— 与 `inode.rs` 同一个取舍
//! （见那里的模块文档第 1 点）。
//!
//! `f_op` 函数指针表换成按 inode 的 [`FsType`] 静态分派（见
//! `fs/mod.rs` 文档第 2 点），所以 `struct file` 里不再有 `f_op` 字段——
//! 读写时从 `f_inode` 的 `i_op`/`i_mode` 决定走哪条路
//! （见 [`super::read_write`]）。
//!
//! `f_reada`（预读标记）保留但不解释：唯一的块设备是 ramdisk，
//! `block_read` 里没有预读（见 `fs/devices.rs` 的说明）。

use crate::fs::NR_FILE;
use crate::fs::inode::NIL;
use crate::pr_info;

/// 一个打开的文件。对应原版 `struct file`。
#[derive(Clone, Copy)]
pub struct File {
    /// 访问模式（`O_RDONLY`/`O_WRONLY`/`O_RDWR` 加权限位）。原版 `mode_t f_mode`
    pub f_mode: u16,
    /// `/dev/tty` 用的设备号。原版 `dev_t f_rdev`
    pub f_rdev: u16,
    /// 读写位置。原版 `off_t f_pos`
    pub f_pos: u64,
    /// 打开标志（`O_APPEND` 等）。原版 `unsigned short f_flags`
    pub f_flags: u32,
    /// 引用计数（`dup`/`fork` 会增）。原版 `unsigned short f_count`
    pub f_count: u16,
    /// 预读标记。原版 `unsigned short f_reada`；见模块文档，保留不解释
    pub f_reada: u16,
    /// 关联的 inode 下标，[`NIL`] 表示这个槽位空闲。原版 `struct inode * f_inode`
    pub f_inode: usize,
}

impl File {
    const fn new() -> Self {
        File {
            f_mode: 0,
            f_rdev: 0,
            f_pos: 0,
            f_flags: 0,
            f_count: 0,
            f_reada: 0,
            f_inode: NIL,
        }
    }
}

/// 打开文件表。原版是动态链表 `first_file`。
static mut FILES: [File; NR_FILE] = [File::new(); NR_FILE];

/// 取一个表项。
///
/// # Safety
/// `n < NR_FILE`；调用者需保证不与其他 `&mut` 别名同时存在。
#[inline]
pub unsafe fn filp(n: usize) -> &'static mut File {
    // SAFETY: 契约保证下标在界内；单核内核。
    unsafe { &mut (*core::ptr::addr_of_mut!(FILES))[n] }
}

/// 找一个空闲表项并把引用计数置 1。对应原版 `get_empty_filp()`。
///
/// 返回 [`NIL`] 表示表满（原版返回 `NULL`；原版在返回 NULL 之前会先
/// `grow_files()`，我们没有扩容）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn get_empty_filp() -> usize {
    // SAFETY: 契约转交；线性扫描定长表。
    unsafe {
        for n in 0..NR_FILE {
            if filp(n).f_count == 0 {
                *filp(n) = File::new();
                filp(n).f_count = 1;
                return n;
            }
        }
        NIL
    }
}

/// 归还一个表项的引用。原版没有独立的函数：`sys_close` 里直接
/// `if (!--filp->f_count) iput(inode)`。包成函数是因为
/// `sys_close`/`sys_dup` 的失败路径与 `do_exit` 都要走同一段逻辑。
///
/// # Safety
/// 只能在进程上下文调用（`iput` 会睡）。
pub unsafe fn put_filp(n: usize) {
    if n == NIL {
        return;
    }
    // SAFETY: 契约转交。
    unsafe {
        let f = filp(n);
        if f.f_count == 0 {
            return;
        }
        f.f_count -= 1;
        if f.f_count == 0 {
            let ino = f.f_inode;
            f.f_inode = NIL;
            super::inode::iput(ino);
        }
    }
}

/// 建立打开文件表。对应原版 `file_table_init()`。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn init() {
    // SAFETY: 契约保证独占。
    unsafe {
        for n in 0..NR_FILE {
            *filp(n) = File::new();
        }
    }
    pr_info!("file table: {} slots", NR_FILE);
}

/// 在用的表项数。自检用。
pub fn nr_used() -> usize {
    // SAFETY: 只读表。
    unsafe { (0..NR_FILE).filter(|&n| filp(n).f_count > 0).count() }
}
