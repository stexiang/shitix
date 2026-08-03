//! 内存类字符设备。对应 linux-1.0.9 的 `drivers/char/mem.c`。
//!
//! 原版按 minor 分派：
//!
//! | minor | 设备 | 原版函数 |
//! |---|---|---|
//! | 1 | `/dev/mem` | `read_mem`/`write_mem`（物理内存）|
//! | 2 | `/dev/kmem` | `read_kmem`（内核虚拟地址空间）|
//! | 3 | `/dev/null` | `read_null`/`write_null` |
//! | 4 | `/dev/port` | `read_port`/`write_port`（I/O 端口）|
//! | 5 | `/dev/zero` | `read_zero`/`write_full` |
//! | 7 | `/dev/full` | 读返回 0，写返回 `-ENOSPC` |
//!
//! 全部移植。`/dev/mem` 与 `/dev/kmem` 在我们这里是同一个东西
//! （恒等映射低 1GB，物理地址就是内核虚拟地址），但仍保留两个 minor
//! 以对齐原版设备号。
//!
//! 原版 `read_mem` 用 `memcpy_tofs` 跨越用户/内核段边界；我们目前所有
//! 代码都跑在内核态（用户态要等 `execve` 移植），所以直接切片拷贝，
//! 并在 [`read`] 里做地址范围检查——这是原版靠段限长免费得到的保护，
//! 我们必须显式做（见 STATUS.md 里 `verify_area` 那条待办）。

use crate::klib::errno::{EFAULT, EINVAL, ENOSPC, ENXIO};
use crate::mm::page_alloc;
use crate::pr_info;

use super::super::block::major::MEM_MAJOR;

/// minor 号。对应原版 `mem.c` 里 `switch (MINOR(inode->i_rdev))` 的分支。
pub mod minor {
    pub const MEM: u32 = 1;
    pub const KMEM: u32 = 2;
    pub const NULL: u32 = 3;
    pub const PORT: u32 = 4;
    pub const ZERO: u32 = 5;
    pub const FULL: u32 = 7;
}

/// 可以直接访问的物理内存上限。恒等映射只到 1GB（见 `boot/setup.S`），
/// 越过这条线的地址在页表里没有映射，碰了就是 page fault。
/// 原版不需要这个检查：`read_mem` 靠 `verify_area` + 段限长挡住。
const IDENTITY_LIMIT: usize = 1 << 30;

/// 从内存类设备读。对应原版 `read_mem`/`read_kmem`/`read_null`/`read_zero`。
///
/// `pos` 是文件偏移，对 `/dev/mem` 就是物理地址。
/// 返回读到的字节数或负 errno。
///
/// # Safety
/// `/dev/mem` 分支会按 `pos` 直接读物理内存。调用者（`fs::read_write`）
/// 必须已确认调用方有权限（原版靠 `/dev/mem` 的 0600 root 属主）。
pub unsafe fn read(mi: u32, pos: u64, buf: &mut [u8]) -> i64 {
    match mi {
        // 原版 read_null：永远返回 0（EOF）
        minor::NULL => 0,
        // 原版 read_zero：填零，返回请求的全长
        minor::ZERO => {
            buf.fill(0);
            buf.len() as i64
        }
        // 原版 /dev/full 读起来跟 /dev/zero 一样
        minor::FULL => {
            buf.fill(0);
            buf.len() as i64
        }
        minor::MEM | minor::KMEM => {
            let start = pos as usize;
            // 原版靠 verify_area；我们只能自己查恒等映射范围（见模块文档）
            let end = match start.checked_add(buf.len()) {
                Some(e) => e,
                None => return -(EFAULT as i64),
            };
            if end > IDENTITY_LIMIT || end > page_alloc::high_memory() {
                return -(EFAULT as i64);
            }
            // SAFETY: 上面已确认 [start, end) 落在恒等映射且不超过实际物理
            // 内存上限，因此这段地址一定有页表映射、读不会触发 page fault。
            unsafe {
                core::ptr::copy_nonoverlapping(start as *const u8, buf.as_mut_ptr(), buf.len());
            }
            buf.len() as i64
        }
        // 原版 read_port：逐字节 inb(pos++)。端口空间只有 64K。
        minor::PORT => {
            let mut n = 0usize;
            let mut p = pos;
            while n < buf.len() {
                if p > 0xffff {
                    break;
                }
                // SAFETY: p 已限制在 16 位端口空间内。读端口本身不会
                // 触发异常；副作用由调用方（root）负责。
                buf[n] = unsafe { inb(p as u16) };
                n += 1;
                p += 1;
            }
            n as i64
        }
        _ => -(ENXIO as i64),
    }
}

/// 往内存类设备写。对应原版 `write_mem`/`write_null`/`write_full`/`write_port`。
///
/// # Safety
/// `/dev/mem` 分支会按 `pos` 直接写物理内存——这可以破坏内核任何数据结构。
/// 同 [`read`]，调用者必须已做权限检查。
pub unsafe fn write(mi: u32, pos: u64, buf: &[u8]) -> i64 {
    match mi {
        // 原版 write_null：吞掉，报告全部写成功
        minor::NULL => buf.len() as i64,
        // 原版 /dev/zero 可写（当 /dev/null 用）
        minor::ZERO => buf.len() as i64,
        // 原版 write_full：永远 -ENOSPC。这正是 /dev/full 存在的意义
        // （给测试程序制造「磁盘满」）
        minor::FULL => -(ENOSPC as i64),
        minor::MEM | minor::KMEM => {
            let start = pos as usize;
            let end = match start.checked_add(buf.len()) {
                Some(e) => e,
                None => return -(EFAULT as i64),
            };
            if end > IDENTITY_LIMIT || end > page_alloc::high_memory() {
                return -(EFAULT as i64);
            }
            // SAFETY: 范围已校验落在恒等映射内。写这段内存可能破坏内核
            // 状态，但那正是 /dev/mem 的语义；权限由调用方保证。
            unsafe {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), start as *mut u8, buf.len());
            }
            buf.len() as i64
        }
        minor::PORT => {
            let mut n = 0usize;
            let mut p = pos;
            while n < buf.len() {
                if p > 0xffff {
                    break;
                }
                // SAFETY: p 限制在 16 位端口空间。写端口的副作用由调用方负责。
                unsafe { outb(p as u16, buf[n]) };
                n += 1;
                p += 1;
            }
            n as i64
        }
        _ => -(ENXIO as i64),
    }
}

/// `lseek` 的语义。对应原版 `memory_lseek()`。
///
/// 原版只支持 `SEEK_SET`(0) 和 `SEEK_CUR`(1)，`SEEK_END` 返回 `-EINVAL`
/// ——内存设备没有「末尾」。照搬。
pub fn lseek(cur: u64, offset: i64, whence: u32) -> i64 {
    let new = match whence {
        crate::fs::SEEK_SET => offset,
        crate::fs::SEEK_CUR => cur as i64 + offset,
        _ => return -(EINVAL as i64),
    };
    if new < 0 {
        return -(EINVAL as i64);
    }
    new
}

/// # Safety
/// `port` 必须合法。
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: 契约转交。
    unsafe { core::arch::asm!("in al, dx", out("al") v, in("dx") port) };
    v
}

/// # Safety
/// `port` 必须合法，且写入的副作用可接受。
#[inline]
unsafe fn outb(port: u16, val: u8) {
    // SAFETY: 契约转交。
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") val) };
}

/// 注册。对应原版 `chr_dev_init` 里的 `memory_init()`
/// （`register_chrdev(MEM_MAJOR, "mem", &memory_fops)`）。
///
/// # Safety
/// 启动期调用一次，需在 `fs::devices::init()` 之后。
pub unsafe fn init() {
    // SAFETY: 契约转交。
    unsafe {
        crate::fs::devices::register_chrdev(
            MEM_MAJOR,
            "mem",
            crate::fs::devices::CharDev::Mem,
        );
    }
    pr_info!("mem: /dev/mem /dev/kmem /dev/null /dev/port /dev/zero /dev/full");
}
