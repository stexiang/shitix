//! 读写与定位。对应 linux-1.0.9 的 `fs/read_write.c`
//! （`sys_read`/`sys_write`/`sys_lseek`/`sys_readdir`）。
//!
//! 原版这个文件很短，因为它只做「查 fd → 检查模式 → 转发到
//! `file->f_op->read/write`」。我们的转发是按 inode 的 [`FsType`]
//! 静态分派（见 `fs/mod.rs` 文档第 2 点），所以那张 `f_op` 表在
//! [`read`] / [`write`] 里表现为一个 `match`。
//!
//! 原版每个 `sys_*` 开头都有 `verify_area(VERIFY_WRITE, buf, count)`：
//! 检查用户缓冲落在进程地址空间内。我们的调用方还都在内核态
//! （见 STATUS.md 里 `verify_area` 那条待办），所以收 Rust 切片——
//! 切片自带长度，越界由类型系统挡住。接上用户态时这里要加回
//! `verify_area` 并把切片换成 `(*mut u8, usize)`。

use crate::fs::file_table::filp;
use crate::fs::inode::{self, FsType, NIL};
use crate::fs::open::fd_to_filp;
use crate::fs::{Dirent, SEEK_CUR, SEEK_END, SEEK_SET, mode, oflags};
use crate::klib::errno::{EBADF, EINVAL, EISDIR, ENOSYS, ENOTDIR, ESPIPE};

/// 读。对应原版 `sys_read()`。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。
pub unsafe fn read(fd: usize, buf: &mut [u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        let (n, pos, fmode) = {
            let fp = filp(f);
            (fp.f_inode, fp.f_pos, fp.f_mode)
        };
        if n == NIL {
            return -(EBADF as i64);
        }
        // 原版：`if (!(file->f_mode & 1)) return -EBADF`（1 = 可读）
        if fmode & 1 == 0 {
            return -(EBADF as i64);
        }
        if buf.is_empty() {
            return 0;
        }

        let m = inode::inode(n).i_mode;
        let r = match inode::inode(n).i_op {
            FsType::Chr => super::devices::chrdev_read(inode::inode(n).i_rdev, pos, buf),
            FsType::Blk => super::devices::block_read(inode::inode(n).i_rdev, pos, buf),
            FsType::Minix => {
                if mode::is_dir(m) {
                    // 原版：目录不能用 read(2) 读，要用 readdir
                    return -(EISDIR as i64);
                }
                super::minix::file::read(n, pos, buf)
            }
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => {
                if mode::is_dir(m) {
                    return -(EISDIR as i64);
                }
                super::ext4::ops::full::ext4_file_read(n, pos, buf)
            }
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => -(ENOSYS as i64),
            FsType::None => -(EINVAL as i64),
        };
        if r > 0 {
            filp(f).f_pos = pos + r as u64;
        }
        r
    }
}

/// 写。对应原版 `sys_write()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn write(fd: usize, buf: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        let (n, fmode, flags) = {
            let fp = filp(f);
            (fp.f_inode, fp.f_mode, fp.f_flags)
        };
        if n == NIL {
            return -(EBADF as i64);
        }
        // 原版：`if (!(file->f_mode & 2)) return -EBADF`（2 = 可写）
        if fmode & 2 == 0 {
            return -(EBADF as i64);
        }
        if buf.is_empty() {
            return 0;
        }

        // O_APPEND：每次写之前都把位置重取到当前末尾。不能只在 open
        // 时取一次——别的进程可能在这中间把文件写长了，那样两边会互相
        // 覆盖。原版在 `minix_file_write` 开头做这件事。
        let pos = if flags & oflags::O_APPEND != 0 {
            inode::inode(n).i_size as u64
        } else {
            filp(f).f_pos
        };

        let m = inode::inode(n).i_mode;
        let iop = inode::inode(n).i_op;
        let r = match iop {
            FsType::Chr => super::devices::chrdev_write(inode::inode(n).i_rdev, pos, buf),
            FsType::Blk => super::devices::block_write(inode::inode(n).i_rdev, pos, buf),
            FsType::Minix => {
                if mode::is_dir(m) {
                    return -(EISDIR as i64);
                }
                super::minix::file::write(n, pos, buf)
            }
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => {
                if mode::is_dir(m) {
                    return -(EISDIR as i64);
                }
                super::ext4::ops::full::ext4_file_write(n, pos, buf)
            }
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => -(ENOSYS as i64),
            FsType::None => -(EINVAL as i64),
        };
        if r > 0 {
            filp(f).f_pos = pos + r as u64;
        }
        r
    }
}

/// 移动读写位置。对应原版 `sys_lseek()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn lseek(fd: usize, offset: i64, whence: u32) -> i64 {
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
        // 原版：管道不能 seek（-ESPIPE）。管道没移植，但字符设备里
        // tty 同样是流式的，seek 无意义。
        if (*inode::inode_ptr(n)).i_op == FsType::Chr
            && super::devices::get_chrfops(super::major(inode::inode(n).i_rdev))
                == Some(super::devices::CharDev::Tty)
        {
            return -(ESPIPE as i64);
        }

        let new = match whence {
            SEEK_SET => offset,
            SEEK_CUR => filp(f).f_pos as i64 + offset,
            SEEK_END => {
                // 块设备的“末尾”是设备容量，不是 i_size（设备文件的
                // i_size 是 0）。原版 block_dev 的 f_op 里 lseek 是 NULL，
                // 走的是默认实现 default_llseek，那里同样特殊处理。
                let end = if (*inode::inode_ptr(n)).i_op == FsType::Blk {
                    match crate::drivers::block::blk_size(super::major(inode::inode(n).i_rdev)) {
                        Some(b) => b as i64 * super::BLOCK_SIZE as i64,
                        None => 0,
                    }
                } else {
                    inode::inode(n).i_size as i64
                };
                end + offset
            }
            _ => return -(EINVAL as i64),
        };
        if new < 0 {
            return -(EINVAL as i64);
        }
        filp(f).f_pos = new as u64;
        // 原版：seek 之后预读标记要清掉
        filp(f).f_reada = 0;
        new
    }
}

/// 读一个目录项。对应原版 `sys_readdir()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn readdir(fd: usize, out: &mut Dirent) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        let (n, pos) = {
            let fp = filp(f);
            (fp.f_inode, fp.f_pos)
        };
        if n == NIL {
            return -(EBADF as i64);
        }
        let ip = inode::inode_ptr(n);
        if (*ip).i_op != FsType::Minix || !mode::is_dir((*ip).i_mode) {
            return -(ENOTDIR as i64);
        }
        let r = super::minix::dir::fill_dirent(n, pos, out);
        if r > 0 {
            filp(f).f_pos = r as u64;
            // 原版 readdir 成功返回 1（读到一项）
            return 1;
        }
        r
    }
}

/// 回写一个 fd 关联的文件。对应原版 `sys_fsync()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn fsync(fd: usize) -> i64 {
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
        // 原版是 file->f_op->fsync；minix 的是 file_fsync = fsync_dev(i_dev)
        let dev = match inode::inode(n).i_op {
            FsType::Blk => inode::inode(n).i_rdev,
            FsType::Minix => inode::inode(n).i_dev,
            _ => return -(EINVAL as i64),
        };
        // inode 本身也要先落盘
        inode::write_inode(n);
        if super::buffer::fsync_dev(dev) { -(crate::klib::errno::EIO as i64) } else { 0 }
    }
}
