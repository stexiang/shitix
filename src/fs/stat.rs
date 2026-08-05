//! 文件状态查询。对应 linux-1.0.9 的 `fs/stat.c`
//! （`sys_stat`/`sys_lstat`/`sys_fstat`）。
//!
//! 原版 `cp_old_stat`/`cp_new_stat` 有两套结构体：`struct old_stat`
//! （libc 4 用，字段更窄）和 `struct new_stat`。我们只实现新的那套
//! （[`Stat`]），字段布局与原版 `struct new_stat` 逐项对应，
//! 这样将来跑原版编译的用户程序能对上。
//!
//! `sys_lstat` 与 `sys_stat` 在我们这里行为相同：区别只在于是否展开
//! 符号链接，而符号链接没移植（见 `minix/mod.rs`）。两个入口都留着，
//! 语义在符号链接到位后自然分开。

use crate::fs::inode::{self, NIL};
use crate::fs::open::fd_to_filp;
use crate::fs::{file_table::filp, namei};
use crate::klib::errno::EBADF;

/// `stat` 的结果。对应原版 `include/linux/stat.h` 的 `struct new_stat`。
///
/// 原版在 i386 上 `unsigned long` 是 32 位；这里保持 32 位字段宽度
/// （与磁盘/ABI 兼容），只有 `st_size` 一类按原版是 `long`。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Stat {
    /// 所在设备。原版 `unsigned short st_dev`
    pub st_dev: u16,
    pub __pad1: u16,
    /// inode 号。原版 `unsigned long st_ino`
    pub st_ino: u32,
    /// 类型与权限。原版 `unsigned short st_mode`
    pub st_mode: u16,
    /// 链接数。原版 `unsigned short st_nlink`
    pub st_nlink: u16,
    pub st_uid: u16,
    pub st_gid: u16,
    /// 设备文件的设备号。原版 `unsigned short st_rdev`
    pub st_rdev: u16,
    pub __pad2: u16,
    /// 大小。原版 `unsigned long st_size`
    pub st_size: u32,
    /// 块大小。原版 `unsigned long st_blksize`
    pub st_blksize: u32,
    /// 占用块数（512 字节单位）。原版 `unsigned long st_blocks`
    pub st_blocks: u32,
    pub st_atime: u32,
    pub __unused1: u32,
    pub st_mtime: u32,
    pub __unused2: u32,
    pub st_ctime: u32,
    pub __unused3: u32,
    pub __unused4: u32,
    pub __unused5: u32,
}

impl Stat {
    pub const fn zeroed() -> Self {
        Stat {
            st_dev: 0,
            __pad1: 0,
            st_ino: 0,
            st_mode: 0,
            st_nlink: 0,
            st_uid: 0,
            st_gid: 0,
            st_rdev: 0,
            __pad2: 0,
            st_size: 0,
            st_blksize: 0,
            st_blocks: 0,
            st_atime: 0,
            __unused1: 0,
            st_mtime: 0,
            __unused2: 0,
            st_ctime: 0,
            __unused3: 0,
            __unused4: 0,
            __unused5: 0,
        }
    }
}

/// 把一个 inode 拷成 [`Stat`]。对应原版 `cp_new_stat()`。
///
/// # Safety
/// `n` 必须是有效 inode 下标。
unsafe fn cp_new_stat(n: usize, out: &mut Stat) {
    // SAFETY: 契约转交。
    unsafe {
        let i = inode::inode(n);
        *out = Stat::zeroed();
        out.st_dev = i.i_dev;
        out.st_ino = i.i_ino;
        out.st_mode = i.i_mode;
        out.st_nlink = i.i_nlink;
        out.st_uid = i.i_uid;
        out.st_gid = i.i_gid;
        out.st_rdev = i.i_rdev;
        out.st_size = i.i_size;
        out.st_blksize = if i.i_blksize == 0 { super::BLOCK_SIZE as u32 } else { i.i_blksize };
        // 原版：st_blocks 是 512 字节块数。minix 的 inode 不存这个
        // （v1 没有 i_blocks 字段），原版从 i_size 算，我们照做。
        out.st_blocks = (i.i_size + 511) / 512;
        out.st_atime = i.i_atime;
        out.st_mtime = i.i_mtime;
        out.st_ctime = i.i_ctime;
    }
}

/// 按路径查。对应原版 `sys_stat()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_stat(path: &[u8], out: &mut Stat) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let n = match namei::namei(path) {
            Ok(n) => n,
            Err(e) => return -(e as i64),
        };
        cp_new_stat(n, out);
        inode::iput(n);
        0
    }
}

/// 不展开符号链接的版本。对应原版 `sys_lstat()`。
/// 见模块文档：目前与 [`sys_stat`] 行为相同。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_lstat(path: &[u8], out: &mut Stat) -> i64 {
    // SAFETY: 契约转交。
    unsafe { sys_stat(path, out) }
}

/// 按 fd 查。对应原版 `sys_fstat()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_fstat(fd: usize, out: &mut Stat) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        let n = filp(f).f_inode;
        if n == NIL {
            return -(EBADF as i64);
        }
        cp_new_stat(n, out);
        0
    }
}
