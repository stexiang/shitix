//! 设备表与设备文件的读写分派。对应 linux-1.0.9 的 `fs/devices.c`
//! 与 `fs/block_dev.c`。
//!
//! # 与原版的结构性差异
//!
//! 原版 `chrdevs[]`/`blkdevs[]` 每项存一个 `struct file_operations *`，
//! 打开设备文件时 `chrdev_open` 把这个指针塞进 `filp->f_op`，之后
//! `sys_read` 就走 `file->f_op->read(...)`。我们改成 [`CharDev`]/[`BlockDev`]
//! 枚举 + 静态分派（见 `fs/mod.rs` 模块文档第 2 点）：设备种类在
//! 编译期就是有限集，枚举比一堆 `Option<fn>` 更好检查完整性，也避免了
//! 「函数指针表里有个 NULL 但调用点忘了判空」这类原版常见的空指针路径。
//!
//! `block_read`/`block_write`（原版 `fs/block_dev.c`）在这里也一并实现：
//! 它们不属于任何具体驱动，是「把对块设备文件的字节流读写翻译成
//! 缓冲缓存操作」的通用代码。

use crate::drivers::block::major::MEM_MAJOR;
use crate::drivers::block::{MAX_BLKDEV, MAX_CHRDEV};
use crate::drivers::char_dev;
use crate::fs::buffer::{self, BLOCK_SIZE, NIL};
use crate::fs::{major, minor};
use crate::klib::errno::{EINVAL, EIO, ENODEV, ENXIO};
use crate::pr_info;

/// 字符设备的种类。原版对应的是 `chrdevs[major].fops` 指向哪张
/// `file_operations` 表。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CharDev {
    /// `drivers/char/tty_io.c` 的 `tty_fops`
    Tty,
    /// `drivers/char/mem.c` 的 `memory_fops`
    Mem,
}

/// 块设备的种类。原版对应 `blkdevs[major].fops`。
///
/// 只有一项：所有块设备的 `file_operations` 在原版里都是
/// `{ NULL, block_read, block_write, …, block_fsync }`——读写都走通用的
/// `block_read`/`block_write`，具体驱动的差异体现在 `blk_dev[].request_fn`
/// 而不是 `f_op`。所以这个枚举其实只需要记「这个 major 注册过没有」，
/// 保留成枚举是为了与 [`CharDev`] 对称，将来加 `/dev/loop` 之类
/// 有特殊 `f_op` 的块设备时有地方放。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BlockDev {
    /// 走通用 `block_read`/`block_write`
    Generic,
}

/// 一个设备表项。对应原版 `struct device_struct { const char * name;
/// struct file_operations * fops; }`。
#[derive(Clone, Copy)]
struct DeviceStruct<T: Copy> {
    name: &'static str,
    kind: Option<T>,
}

impl<T: Copy> DeviceStruct<T> {
    const fn new() -> Self {
        DeviceStruct { name: "", kind: None }
    }
}

/// 对应原版 `static struct device_struct chrdevs[MAX_CHRDEV]`。
static mut CHRDEVS: [DeviceStruct<CharDev>; MAX_CHRDEV] =
    [DeviceStruct::new(); MAX_CHRDEV];

/// 对应原版 `static struct device_struct blkdevs[MAX_BLKDEV]`。
static mut BLKDEVS: [DeviceStruct<BlockDev>; MAX_BLKDEV] =
    [DeviceStruct::new(); MAX_BLKDEV];

/// 对应原版 `get_chrfops()`。
pub fn get_chrfops(ma: u32) -> Option<CharDev> {
    if ma as usize >= MAX_CHRDEV {
        return None;
    }
    // SAFETY: 已查界；表在启动期建好后只读。
    unsafe { (*core::ptr::addr_of!(CHRDEVS))[ma as usize].kind }
}

/// 对应原版 `get_blkfops()`。
pub fn get_blkfops(ma: u32) -> Option<BlockDev> {
    if ma as usize >= MAX_BLKDEV {
        return None;
    }
    // SAFETY: 已查界；同上。
    unsafe { (*core::ptr::addr_of!(BLKDEVS))[ma as usize].kind }
}

/// 对应原版 `register_chrdev()`。已被占用返回 `-EBUSY`（这里用 bool）。
///
/// # Safety
/// 启动期调用（原版允许模块在运行时注册，我们没有模块）。
pub unsafe fn register_chrdev(ma: u32, name: &'static str, kind: CharDev) -> bool {
    if ma as usize >= MAX_CHRDEV {
        return false;
    }
    // SAFETY: 已查界；启动期无并发。
    unsafe {
        let e = &mut (*core::ptr::addr_of_mut!(CHRDEVS))[ma as usize];
        if e.kind.is_some() {
            return false; // 原版 -EBUSY
        }
        e.name = name;
        e.kind = Some(kind);
    }
    true
}

/// 对应原版 `register_blkdev()`。
///
/// # Safety
/// 同 [`register_chrdev`]。
pub unsafe fn register_blkdev(ma: u32, name: &'static str, kind: BlockDev) -> bool {
    if ma as usize >= MAX_BLKDEV {
        return false;
    }
    // SAFETY: 已查界；启动期无并发。
    unsafe {
        let e = &mut (*core::ptr::addr_of_mut!(BLKDEVS))[ma as usize];
        if e.kind.is_some() {
            return false;
        }
        e.name = name;
        e.kind = Some(kind);
    }
    true
}

/// 打开一个块设备文件。对应原版 `blkdev_open()`。
/// 返回 0 或负 errno。
pub fn blkdev_open(rdev: u16) -> i64 {
    match get_blkfops(major(rdev)) {
        Some(_) => 0,
        None => -(ENODEV as i64),
    }
}

/// 打开一个字符设备文件。对应原版 `chrdev_open()`。
pub fn chrdev_open(rdev: u16) -> i64 {
    match get_chrfops(major(rdev)) {
        Some(_) => 0,
        None => -(ENODEV as i64),
    }
}

// ---- 字符设备读写（原版各驱动的 f_op->read/write）----

/// 从字符设备读。对应原版 `char_read()`（`fs/read_write.c` 里那个转发
/// 到 `file->f_op->read` 的桩）。
///
/// # Safety
/// 只能在进程上下文调用（tty 会睡）。`/dev/mem` 分支的安全性见
/// [`char_dev::mem::read`]。
pub unsafe fn chrdev_read(rdev: u16, pos: u64, buf: &mut [u8]) -> i64 {
    // SAFETY: 契约转交给具体驱动。
    unsafe {
        match get_chrfops(major(rdev)) {
            Some(CharDev::Tty) => char_dev::tty::tty_read(buf),
            Some(CharDev::Mem) => char_dev::mem::read(minor(rdev), pos, buf),
            None => -(ENXIO as i64),
        }
    }
}

/// 往字符设备写。对应原版 `char_write()`。
///
/// # Safety
/// 同 [`chrdev_read`]。
pub unsafe fn chrdev_write(rdev: u16, pos: u64, buf: &[u8]) -> i64 {
    // SAFETY: 契约转交给具体驱动。
    unsafe {
        let ma = major(rdev);
        let mi = minor(rdev);
        crate::serial::print("CHR: major=");
        crate::serial::print_dec(ma as u64);
        crate::serial::print(" minor=");
        crate::serial::print_dec(mi as u64);
        crate::serial::putc(b'\n');
        match get_chrfops(ma) {
            Some(CharDev::Tty) => {
                crate::serial::print("CHR: -> tty_write\n");
                char_dev::tty::tty_write(buf)
            }
            Some(CharDev::Mem) => {
                crate::serial::print("CHR: -> mem_write\n");
                char_dev::mem::write(mi, pos, buf)
            }
            None => {
                crate::serial::print("CHR: no driver\n");
                -(ENXIO as i64)
            }
        }
    }
}

// ---- 块设备文件的字节流读写（原版 fs/block_dev.c）----

/// 按字节流读一个块设备文件。对应原版 `block_read()`。
///
/// 原版做了预读（`read_ahead[MAJOR]` 与 `breada`）和「一次提交多个块」
/// 的优化，还处理 `blocksize != BLOCK_SIZE` 的情况。我们逐块 `bread`：
/// 唯一的块设备是 ramdisk，预读没有收益（没有寻道），而多块提交在
/// `ll_rw_block` 那层已经支持、只是这里用不上。
///
/// 返回读到的字节数或负 errno。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。
pub unsafe fn block_read(dev: u16, pos: u64, buf: &mut [u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        // 设备容量（1024 字节块数）。原版用 blk_size[MAJOR][MINOR]。
        let size_bytes = match crate::drivers::block::blk_size(major(dev)) {
            Some(blocks) => blocks as u64 * BLOCK_SIZE as u64,
            // 原版 `!blk_size[MAJOR]` 表示不检查；我们没有这样的设备
            None => return -(ENXIO as i64),
        };
        if pos >= size_bytes {
            return 0; // EOF
        }
        let want = buf.len().min((size_bytes - pos) as usize);

        let mut done = 0usize;
        while done < want {
            let off = pos + done as u64;
            let block = (off / BLOCK_SIZE as u64) as u32;
            let in_block = (off % BLOCK_SIZE as u64) as usize;
            let chunk = (BLOCK_SIZE - in_block).min(want - done);

            let n = match buffer::bread(dev, block, BLOCK_SIZE) {
                Some(n) => n,
                None => {
                    // 原版：读失败时已读到的部分照样返回，一个字节都没读到才报错
                    return if done > 0 { done as i64 } else { -(EIO as i64) };
                }
            };
            buf[done..done + chunk]
                .copy_from_slice(&buffer::bh(n).data()[in_block..in_block + chunk]);
            buffer::brelse(n);
            done += chunk;
        }
        done as i64
    }
}

/// 按字节流写一个块设备文件。对应原版 `block_write()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn block_write(dev: u16, pos: u64, buf: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let size_bytes = match crate::drivers::block::blk_size(major(dev)) {
            Some(blocks) => blocks as u64 * BLOCK_SIZE as u64,
            None => return -(ENXIO as i64),
        };
        if pos >= size_bytes {
            // 原版 block_write 在越界时返回 -ENOSPC
            return -(EINVAL as i64);
        }
        let want = buf.len().min((size_bytes - pos) as usize);

        let mut done = 0usize;
        while done < want {
            let off = pos + done as u64;
            let block = (off / BLOCK_SIZE as u64) as u32;
            let in_block = (off % BLOCK_SIZE as u64) as usize;
            let chunk = (BLOCK_SIZE - in_block).min(want - done);

            // 整块覆盖时不必先读（原版同样有这个判断：
            // `if (block_size == BLOCK_SIZE) bh = getblk(...) else bh = bread(...)`）
            let n = if chunk == BLOCK_SIZE {
                match buffer::getblk(dev, block, BLOCK_SIZE) {
                    Some(n) => n,
                    None => return if done > 0 { done as i64 } else { -(EIO as i64) },
                }
            } else {
                match buffer::bread(dev, block, BLOCK_SIZE) {
                    Some(n) => n,
                    None => return if done > 0 { done as i64 } else { -(EIO as i64) },
                }
            };
            buffer::bh(n).data_mut()[in_block..in_block + chunk]
                .copy_from_slice(&buf[done..done + chunk]);
            buffer::bh(n).b_uptodate = true;
            buffer::mark_buffer_dirty(n);
            buffer::brelse(n);
            done += chunk;
        }
        done as i64
    }
}

/// 块设备的 `fsync`。对应原版 `block_fsync()`（就是 `fsync_dev`）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn block_fsync(dev: u16) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        if buffer::fsync_dev(dev) { -(EIO as i64) } else { 0 }
    }
}

/// 建立设备表并注册块设备。对应原版 `fs/devices.c` 里那两张表的
/// 静态初始化 + 各驱动的 `register_blkdev`。
///
/// tty 与 mem 的 `register_chrdev` 在它们各自的 `init()` 里
/// （同原版：`tty_init`/`memory_init` 自己注册）。
///
/// # Safety
/// 启动期调用一次，需在驱动 `init()` 之前。
pub unsafe fn init() {
    // SAFETY: 契约保证独占。
    unsafe {
        for i in 0..MAX_CHRDEV {
            (*core::ptr::addr_of_mut!(CHRDEVS))[i] = DeviceStruct::new();
        }
        for i in 0..MAX_BLKDEV {
            (*core::ptr::addr_of_mut!(BLKDEVS))[i] = DeviceStruct::new();
        }
        // ramdisk 走通用 block_read/block_write（原版 rd_fops 就是这样）
        register_blkdev(MEM_MAJOR, "rd", BlockDev::Generic);
    }
    pr_info!("devices: {} chrdev slots, {} blkdev slots", MAX_CHRDEV, MAX_BLKDEV);
}

/// 消掉 `NIL` 的未使用告警。
#[allow(dead_code)]
const _NIL: usize = NIL;
