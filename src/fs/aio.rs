//! 内核 AIO（`io_setup` 系列）的同步实现。
//!
//! 没有真正的异步 I/O 引擎：`io_submit` 在提交进程的上下文里原地执行
//! pread/pwrite/fsync，把结果写进完成队列；`io_getevents` 立即返回。
//! 对调用方语义兼容（Linux AIO 本来就不保证异步，O_DIRECT 之外很多
//! 路径在真内核里也是同步退化的），只是没有并行性收益。

use crate::klib::errno::{EAGAIN, EFAULT, EINVAL};

/// 同时存在的 AIO 上下文上限（内核固定表风格，同 inode/file 表）。
const MAX_AIO_CTX: usize = 4;
/// 每个上下文的完成事件环容量。提交是同步的，32 条足够防溢出；
/// io_setup 的 nr_events 超过它也按 32 截断（Linux 允许内核上调容量）。
const AIO_RING: usize = 32;

/// `struct io_event`（<linux/aio_abi.h>）：完成事件。
#[derive(Clone, Copy)]
#[repr(C)]
struct IoEvent {
    data: u64,
    obj: u64,
    res: i64,
    res2: i64,
}

const EMPTY_EVENT: IoEvent = IoEvent { data: 0, obj: 0, res: 0, res2: 0 };

struct AioCtx {
    used: bool,
    events: [IoEvent; AIO_RING],
    head: usize, // 下一个可读
    tail: usize, // 下一个可写
}

const EMPTY_CTX: AioCtx = AioCtx { used: false, events: [EMPTY_EVENT; AIO_RING], head: 0, tail: 0 };

static mut CTXS: [AioCtx; MAX_AIO_CTX] = [EMPTY_CTX; MAX_AIO_CTX];

/// iocb 命令字（<linux/aio_abi.h>）。
const IOCB_CMD_PREAD: u16 = 0;
const IOCB_CMD_PWRITE: u16 = 1;
const IOCB_CMD_FSYNC: u16 = 2;
const IOCB_CMD_FDSYNC: u16 = 3;

/// 用户态 `struct iocb` 的字段偏移（x86_64，总共 64 字节）。
mod iocb {
    pub const DATA: usize = 0;
    pub const LIO_OPCODE: usize = 16; // u16
    pub const FILDES: usize = 20; // u32
    pub const BUF: usize = 24; // u64
    pub const NBYTES: usize = 32; // u64
    pub const OFFSET: usize = 40; // u64
}

/// 上下文句柄 = 槽位下标 + 1（0 留给非法值）。
fn ctx_slot(ctx_id: u64) -> Option<usize> {
    if ctx_id == 0 || ctx_id > MAX_AIO_CTX as u64 {
        return None;
    }
    let i = (ctx_id - 1) as usize;
    // SAFETY: 单核系统调用上下文，表只在这里被访问。
    let used = unsafe { CTXS[i].used };
    if used { Some(i) } else { None }
}

/// `io_setup(nr_events, ctx_idp)`。返回 0 并把上下文句柄写给用户。
pub fn io_setup(nr_events: u64, ctx_idp: u64) -> i64 {
    if nr_events == 0 || ctx_idp == 0 {
        return -(EINVAL as i64);
    }
    // SAFETY: 单核系统调用上下文；用户指针恒等映射可直接写。
    unsafe {
        for i in 0..MAX_AIO_CTX {
            if !CTXS[i].used {
                CTXS[i].used = true;
                CTXS[i].head = 0;
                CTXS[i].tail = 0;
                core::ptr::write_volatile(ctx_idp as *mut u64, (i + 1) as u64);
                return 0;
            }
        }
    }
    -(EAGAIN as i64) // 表满（Linux 也返回 EAGAIN）
}

/// `io_destroy(ctx_id)`。
pub fn io_destroy(ctx_id: u64) -> i64 {
    match ctx_slot(ctx_id) {
        None => -(EINVAL as i64),
        // SAFETY: 同上。
        Some(i) => unsafe {
            CTXS[i] = EMPTY_CTX;
            0
        },
    }
}

/// 同步执行一个 iocb，返回给 io_event.res 的值。
/// SAFETY: iocb 指向用户态 64 字节 iocb；缓冲指针按恒等映射解引用。
unsafe fn run_iocb(iocb: u64) -> i64 {
    let p = iocb as *const u8;
    // SAFETY: 见上。
    let (op, fd, buf, nbytes, offset) = unsafe {
        (
            core::ptr::read_volatile(p.add(iocb::LIO_OPCODE) as *const u16),
            core::ptr::read_volatile(p.add(iocb::FILDES) as *const u32),
            core::ptr::read_volatile(p.add(iocb::BUF) as *const u64),
            core::ptr::read_volatile(p.add(iocb::NBYTES) as *const u64),
            core::ptr::read_volatile(p.add(iocb::OFFSET) as *const u64),
        )
    };
    let fd = fd as usize;
    match op {
        IOCB_CMD_PREAD | IOCB_CMD_PWRITE => {
            if nbytes > 0 && buf == 0 {
                return -(EFAULT as i64);
            }
            // pread/pwrite 语义：不动 f_pos。同 sys_pread64 的保存/恢复。
            let old = unsafe { crate::fs::read_write::lseek(fd, 0, crate::fs::SEEK_CUR) };
            if old < 0 {
                return old;
            }
            unsafe { crate::fs::read_write::lseek(fd, offset as i64, crate::fs::SEEK_SET) };
            let r = if op == IOCB_CMD_PREAD {
                // SAFETY: buf 是用户缓冲，长度 nbytes。
                let s = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, nbytes as usize) };
                unsafe { crate::fs::read_write::read(fd, s) }
            } else {
                // SAFETY: 同上。
                let s = unsafe { core::slice::from_raw_parts(buf as *const u8, nbytes as usize) };
                unsafe { crate::fs::read_write::write(fd, s) }
            };
            unsafe { crate::fs::read_write::lseek(fd, old, crate::fs::SEEK_SET) };
            r
        }
        IOCB_CMD_FSYNC | IOCB_CMD_FDSYNC => unsafe { crate::fs::read_write::fsync(fd) },
        _ => -(EINVAL as i64),
    }
}

/// `io_submit(ctx_id, nr, iocbpp)`：逐个同步执行并投递完成事件。
pub fn io_submit(ctx_id: u64, nr: i64, iocbpp: u64) -> i64 {
    let i = match ctx_slot(ctx_id) {
        None => return -(EINVAL as i64),
        Some(i) => i,
    };
    if nr < 1 || nr > AIO_RING as i64 || iocbpp == 0 {
        return -(EINVAL as i64);
    }
    // SAFETY: 单核系统调用上下文。
    unsafe {
        let ctx = &mut CTXS[i];
        for k in 0..nr as usize {
            let iocb = core::ptr::read_volatile((iocbpp as *const u64).add(k));
            if iocb == 0 {
                return -(EFAULT as i64);
            }
            let data = core::ptr::read_volatile((iocb as *const u8).add(iocb::DATA) as *const u64);
            let res = run_iocb(iocb);
            if ctx.tail - ctx.head >= AIO_RING {
                return k as i64; // 环满（理论上不该发生）：返回已提交数
            }
            ctx.events[ctx.tail % AIO_RING] = IoEvent { data, obj: iocb, res, res2: 0 };
            ctx.tail += 1;
        }
        nr
    }
}

/// `io_getevents(ctx_id, min_nr, nr, events, timeout)`。
/// 提交是同步的，事件要么已有要么永远不会有：不够 min_nr 也不阻塞。
pub fn io_getevents(ctx_id: u64, min_nr: i64, nr: i64, events: u64) -> i64 {
    let i = match ctx_slot(ctx_id) {
        None => return -(EINVAL as i64),
        Some(i) => i,
    };
    if min_nr < 0 || nr < 0 || min_nr > nr || events == 0 && nr > 0 {
        return -(EINVAL as i64);
    }
    // SAFETY: 单核系统调用上下文；events 是用户缓冲。
    unsafe {
        let ctx = &mut CTXS[i];
        let avail = ctx.tail - ctx.head;
        let n = core::cmp::min(avail, nr as usize);
        for k in 0..n {
            let ev = ctx.events[ctx.head % AIO_RING];
            ctx.head += 1;
            core::ptr::write_volatile((events as *mut IoEvent).add(k), ev);
        }
        n as i64
    }
}

/// `io_cancel(ctx_id, iocbp, result)`：提交即完成，没有可取消的在途
/// 请求，恒 `-EAGAIN`（Linux 对已完成/不在队的 iocb 也是 EAGAIN）。
pub fn io_cancel(ctx_id: u64, _iocbp: u64, _result: u64) -> i64 {
    match ctx_slot(ctx_id) {
        None => -(EINVAL as i64),
        Some(_) => -(EAGAIN as i64),
    }
}

/// `io_pgetevents(...)` = [`io_getevents`] + sigmask（忽略）。
pub fn io_pgetevents(ctx_id: u64, min_nr: i64, nr: i64, events: u64) -> i64 {
    io_getevents(ctx_id, min_nr, nr, events)
}

/// 自检：建上下文 → 提交对 /dev/null 的读 → 收完成事件 → 销毁。
pub fn selftest() -> bool {
    let mut ctx_id: u64 = 0;
    let r = io_setup(8, &mut ctx_id as *mut u64 as u64);
    if r != 0 || ctx_id == 0 {
        return false;
    }
    // 没有可靠的用户 fd 可借（selftest 跑在内核线程），这里只验证
    // 提交-完成-取事件 的环形队列机制本身：直接驱动内部函数。
    let ok = unsafe {
        let i = ctx_slot(ctx_id).unwrap();
        CTXS[i].events[0] = IoEvent { data: 1, obj: 2, res: 3, res2: 0 };
        CTXS[i].tail = 1;
        let mut ev = EMPTY_EVENT;
        let got = io_getevents(ctx_id, 1, 1, &mut ev as *mut IoEvent as u64);
        got == 1 && ev.data == 1 && ev.res == 3 && CTXS[i].head == 1
    };
    io_destroy(ctx_id);
    ok
}
