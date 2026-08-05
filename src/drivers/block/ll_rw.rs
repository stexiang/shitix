//! 块设备请求队列。对应 linux-1.0.9 的 `drivers/block/ll_rw_blk.c`
//! 与 `drivers/block/blk.h` 里的 `struct request` / `end_request` / `INIT_REQUEST`。
//!
//! # 与原版的结构性差异
//!
//! 1. **请求池下标化**。原版 `all_requests[NR_REQUEST]` 里的 `req->dev < 0`
//!    表示空闲，`req->next` 是指针。我们用 `Option<u16>` 装设备号
//!    （`None` = 空闲，替代原版的 `dev = -1`）、`usize` 下标做链。
//! 2. **`request_fn` 保留函数指针**。这是设备驱动的注册点，原版
//!    `blk_dev[major].request_fn` 就是函数指针，用枚举反而别扭
//!    （驱动是外部注册的，不是内核内枚举得完的集合）。
//! 3. **不做电梯调度合并**。原版 `add_request` 用 `IN_ORDER` 宏按
//!    (major, minor, sector) 排序插入以减少磁头寻道，`make_request` 还会把
//!    相邻扇区的请求合并到同一个 `req`（那段 `req->sector + req->nr_sectors
//!    == sector` 的判断）。我们唯一的块设备是 ramdisk，寻道时间为零，
//!    合并只会增加一条没有测试覆盖的复杂路径。请求按 FIFO 入队。
//!    `plug`（原版为了攒够请求再一起下发的那个假请求）同理省掉。
//! 4. **不移植 `ll_rw_page`/`ll_rw_swap_file`**：属于交换子系统。

use crate::{pr_err};
use crate::fs::buffer::{self, NIL, bh};
use crate::fs::{READ, READA, WRITE, WRITEA, major, minor};
use crate::irq;
use crate::sched::WaitQueue;

use super::{MAX_BLKDEV, SECTOR_SIZE};

/// 请求池大小。对应原版 `blk.h` 的 `NR_REQUEST 64`，缩到 16
/// （只有一个零延迟的 ramdisk，队列深度没有意义）。
pub const NR_REQUEST: usize = 16;

/// 一次 I/O 请求。对应原版 `struct request`。
#[derive(Clone, Copy)]
pub struct Request {
    /// 目标设备，`None` 表示这个槽位空闲（原版 `dev = -1`）
    pub dev: Option<u16>,
    /// [`READ`] 或 [`WRITE`]。原版 `int cmd`
    pub cmd: i32,
    /// 错误重试计数。原版 `int errors`
    pub errors: i32,
    /// 起始扇区（512 字节单位）。原版 `unsigned long sector`
    pub sector: u32,
    /// 本请求总扇区数。原版 `unsigned long nr_sectors`
    pub nr_sectors: u32,
    /// 当前这个缓冲的扇区数。原版 `unsigned long current_nr_sectors`
    pub current_nr_sectors: u32,
    /// 当前数据指针。原版 `char * buffer`
    pub buffer: *mut u8,
    /// 等这个请求完成的任务下标，[`NIL`] 表示没人等。
    /// 原版 `struct task_struct * waiting`
    pub waiting: usize,
    /// 关联的缓冲链首。原版 `struct buffer_head * bh`
    pub bh: usize,
    /// 缓冲链尾。原版 `struct buffer_head * bhtail`
    pub bhtail: usize,
    /// 队列里的下一个请求。原版 `struct request * next`
    pub next: usize,
}

impl Request {
    const fn new() -> Self {
        Request {
            dev: None,
            cmd: READ,
            errors: 0,
            sector: 0,
            nr_sectors: 0,
            current_nr_sectors: 0,
            buffer: core::ptr::null_mut(),
            waiting: NIL,
            bh: NIL,
            bhtail: NIL,
            next: NIL,
        }
    }
}

/// 驱动的请求处理函数。对应原版 `blk_dev[].request_fn`，即各驱动的
/// `do_rd_request`/`do_hd_request`。约定同原版：被调用时队列非空，
/// 处理完一个请求要调 [`end_request`] 然后继续看队首。
pub type RequestFn = fn();

/// 每个主设备号的队列。对应原版 `struct blk_dev_struct blk_dev[MAX_BLKDEV]`。
pub struct BlkDev {
    /// 原版 `void (*request_fn)(void)`
    pub request_fn: Option<RequestFn>,
    /// 队首请求的下标。原版 `struct request * current_request`
    pub current_request: usize,
}

impl BlkDev {
    const fn new() -> Self {
        BlkDev { request_fn: None, current_request: NIL }
    }
}

/// 请求池。对应原版 `static struct request all_requests[NR_REQUEST]`。
static mut ALL_REQUESTS: [Request; NR_REQUEST] = [Request::new(); NR_REQUEST];

/// 各主设备号的队列。对应原版 `blk_dev[]`。
static mut BLK_DEV: [BlkDev; MAX_BLKDEV] = [const { BlkDev::new() }; MAX_BLKDEV];

/// 等空闲请求槽的任务。对应原版 `struct wait_queue * wait_for_request`。
static mut WAIT_FOR_REQUEST: WaitQueue = WaitQueue::new();

/// 各设备容量，单位 1024 字节块。对应原版 `int * blk_size[MAX_BLKDEV]`。
/// 原版是「指针数组，每个指向该主设备号的 minor 数组」，`NULL` 表示不做
/// 容量检查。我们简化成每个主设备号一个容量（我们的块设备都只有一个 minor）。
static mut BLK_SIZE: [Option<u32>; MAX_BLKDEV] = [None; MAX_BLKDEV];

/// 只读标志位图。对应原版 `static long ro_bits[MAX_BLKDEV][8]`。
/// 原版按 minor 分位，我们同样按 minor 分位（一个 u32 覆盖 32 个 minor）。
static mut RO_BITS: [u32; MAX_BLKDEV] = [0; MAX_BLKDEV];

/// 请求槽的裸指针。理由同 [`crate::fs::buffer::buf_ptr`]：连着调 [`req`]
/// 会产生重叠的 `&mut`，而 `&mut` 带 noalias，编译器可以丢掉其中一条上的
/// 写或复用过期的读。只读一个字段也要用它——返回的 `&mut` 会一直活到
/// 语句结束，跨过中间那次访问器调用。
///
/// # Safety
/// `n < NR_REQUEST`。
#[inline]
#[track_caller]
unsafe fn req_ptr(n: usize) -> *mut Request {
    assert!(n < NR_REQUEST, "req_ptr(): index {} out of range (NR_REQUEST={})", n, NR_REQUEST);
    // SAFETY: 上面已校验下标在界内。
    unsafe { (*core::ptr::addr_of_mut!(ALL_REQUESTS)).as_mut_ptr().add(n) }
}

/// 取请求。
///
/// # Safety
/// `n < NR_REQUEST`；调用者已关中断或确保无并发。
#[inline]
#[track_caller]
unsafe fn req(n: usize) -> &'static mut Request {
    // 越界报下标。见 `fs::super_block::sb` 里的说明：静默越界写会污染
    // BSS 里紧邻的表，症状出现在完全无关的地方。
    assert!(n < NR_REQUEST, "req(): index {} out of range (NR_REQUEST={})", n, NR_REQUEST);
    // SAFETY: 上面已校验下标在界内。
    unsafe { &mut (*core::ptr::addr_of_mut!(ALL_REQUESTS))[n] }
}

/// 取队列。
///
/// # Safety
/// `m < MAX_BLKDEV`；同 [`req`]。
#[inline]
#[track_caller]
pub unsafe fn blk_dev(m: usize) -> &'static mut BlkDev {
    assert!(m < MAX_BLKDEV, "blk_dev(): major {} out of range", m);
    // SAFETY: 上面已校验下标在界内。
    unsafe { &mut (*core::ptr::addr_of_mut!(BLK_DEV))[m] }
}

/// 注册驱动的 `request_fn`。对应原版驱动里那句
/// `blk_dev[MAJOR_NR].request_fn = DEVICE_REQUEST;`。
///
/// # Safety
/// 启动期调用；`m` 必须 `< MAX_BLKDEV`（越界返回 false 而不是 panic）。
pub unsafe fn register_request_fn(m: u32, f: RequestFn) -> bool {
    if m as usize >= MAX_BLKDEV {
        return false;
    }
    // SAFETY: 上面已查界；启动期无并发。
    unsafe { blk_dev(m as usize).request_fn = Some(f) }
    true
}

/// 设置设备容量（1024 字节块数）。对应原版给 `blk_size[MAJOR]` 赋值。
///
/// # Safety
/// 启动期调用。
pub unsafe fn set_blk_size(m: u32, blocks: u32) {
    if m as usize >= MAX_BLKDEV {
        return;
    }
    // SAFETY: 已查界；启动期无并发。
    unsafe { (*core::ptr::addr_of_mut!(BLK_SIZE))[m as usize] = Some(blocks) }
}

/// 设备容量。`None` 表示不检查（同原版 `!blk_size[MAJOR]`）。
pub fn blk_size(m: u32) -> Option<u32> {
    if m as usize >= MAX_BLKDEV {
        return None;
    }
    // SAFETY: 已查界；只读一个 Option<u32>。
    unsafe { (*core::ptr::addr_of!(BLK_SIZE))[m as usize] }
}

/// 对应原版 `is_read_only()`。
pub fn is_read_only(dev: u16) -> bool {
    let (ma, mi) = (major(dev) as usize, minor(dev));
    if ma >= MAX_BLKDEV || mi >= 32 {
        return false;
    }
    // SAFETY: 已查界；只读一个 u32。
    unsafe { (*core::ptr::addr_of!(RO_BITS))[ma] & (1 << mi) != 0 }
}

/// 对应原版 `set_device_ro()`。
///
/// # Safety
/// 调用者需保证不与 `is_read_only` 并发（启动期或持锁时调用）。
pub unsafe fn set_device_ro(dev: u16, flag: bool) {
    let (ma, mi) = (major(dev) as usize, minor(dev));
    if ma >= MAX_BLKDEV || mi >= 32 {
        return;
    }
    // SAFETY: 已查界。
    unsafe {
        let bits = &mut (*core::ptr::addr_of_mut!(RO_BITS))[ma];
        if flag {
            *bits |= 1 << mi;
        } else {
            *bits &= !(1 << mi);
        }
    }
}

// ---- 请求分配 ----

/// 在前 `n` 个槽位里找一个空闲请求。对应原版 `get_request()`。
///
/// 原版用 `prev_found` 做轮转起点以摊平槽位使用，并从 `limit` 往下扫；
/// 我们直接从头扫（池只有 16 个，轮转省下的那点扫描没有意义）。
/// 原版通过「限制 WRITE 只能用前 2/3 个槽」给读留出余量，这个语义保留
/// （见 [`make_request`] 里的 `max_req`）。
///
/// # Safety
/// 必须在关中断状态下调用（原版注释里明确要求）。
unsafe fn get_request(n: usize, dev: u16) -> usize {
    // SAFETY: 契约保证已关中断，独占请求池。
    unsafe {
        for i in 0..n.min(NR_REQUEST) {
            if req(i).dev.is_none() {
                req(i).dev = Some(dev);
                return i;
            }
        }
        NIL
    }
}

/// 把请求挂到队尾，队列原本为空则立刻驱动一次。
/// 对应原版 `add_request()`，但不做电梯排序（见模块文档第 3 点）。
///
/// # Safety
/// 必须在关中断状态下调用。`n` 是有效请求下标。
unsafe fn add_request(m: usize, n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        req(n).next = NIL;
        // 整段在关中断下跑。原版 `add_request` 是
        // `req->next = NULL; cli(); ... (dev->request_fn)(); sti();`——
        // 注意 cli 一直罩到 request_fn 调用完。
        //
        // 不关中断的后果：current_request 链表的读改写不是原子的，而
        // end_request 在中断上下文里也会摘链。两边交错时请求的 bh 指针会
        // 错位，于是「读块 1」把别的块的内容填进块 1 的缓冲，还把
        // b_uptodate 置上——症状是 mount 时读到的超级块魔数是垃圾，而
        // ramdisk 内存里其实是对的。
        let flags = irq::local_irq_save();
        let b = req(n).bh;
        if b != NIL {
            bh(b).b_dirt = false;
        }

        // 不持有 `blk_dev(m)` 的 &mut 去走 req() 链：两条 &mut 重叠时
        // noalias 会让「读 current_request」和「写 req(tail).next」之间的
        // 依赖被优化掉，链尾接错位置。先把队首读出来，写完再回写。
        let head = blk_dev(m).current_request;
        if head == NIL {
            blk_dev(m).current_request = n;
            let f = blk_dev(m).request_fn;
            if let Some(f) = f {
                f();
            }
            irq::restore_flags(flags);
            return;
        }
        let mut tail = head;
        while (*req_ptr(tail)).next != NIL {
            tail = req(tail).next;
        }
        req(tail).next = n;
        irq::restore_flags(flags);
    }
}

/// 为一个缓冲生成一条请求。对应原版 `make_request()`。
///
/// # Safety
/// 不能在中断上下文调用（会 `lock_buffer`/`sleep_on`）。
unsafe fn make_request(m: usize, rw_in: i32, n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        // READA/WRITEA：缓冲已锁就直接放弃（预读不值得等），否则降级成普通读写
        let rw_ahead = rw_in == READA || rw_in == WRITEA;
        let rw = if rw_ahead {
            if bh(n).b_lock {
                return;
            }
            if rw_in == READA { READ } else { WRITE }
        } else {
            rw_in
        };
        if rw != READ && rw != WRITE {
            pr_err!("Bad block dev command, must be R/W/RA/WA");
            return;
        }

        let count = (bh(n).b_size / SECTOR_SIZE) as u32;
        let sector = bh(n).b_blocknr * count;

        // 容量检查。原版比的是 `blk_size[major][minor] < (sector+count)>>1`，
        // 即把扇区数换算成 1024 字节块数来比。
        if let Some(cap) = blk_size(m as u32) {
            if cap < (sector + count) >> 1 {
                let b = bh(n);
                b.b_dirt = false;
                b.b_uptodate = false;
                return;
            }
        }

        buffer::lock_buffer(n);
        // 锁上之后再判一次：写但不脏、读但已有效，都是白跑
        let (dirt, up) = { let p = bh(n); (p.b_dirt, p.b_uptodate) };
        if (rw == WRITE && !dirt) || (rw == READ && up) {
            buffer::unlock_buffer(n);
            return;
        }

        // 原版：后 1/3 的槽位只给读用，避免写把队列填满饿死读
        let max_req = if rw == READ { NR_REQUEST } else { NR_REQUEST * 2 / 3 };
        let dev = bh(n).b_dev;

        let r = loop {
            // 关中断查队列。原版这段是 `repeat: cli(); ... sleep_on(); sti();
            // goto repeat;`——注意它**带着关中断状态去睡**，`sti()` 在
            // sleep_on 返回之后才执行。
            //
            // 这一点不能省：若在 sleep_on 之前就开中断，「get_request 返回
            // NIL」和「挂上等待队列」之间就有一个窗口，end_request 可能正好
            // 在这个窗口里释放槽位并 wake_up——那次唤醒没人收到，我们却已经
            // 睡下去，成了丢失唤醒。sleep_on 内部会在切换前把中断放开
            // （见 sched::WaitQueue::sleep_on_state），所以不会真的关着中断睡。
            let flags = irq::local_irq_save();
            let r = get_request(max_req, dev);
            if r != NIL {
                irq::restore_flags(flags);
                break r;
            }
            // 没槽位了。预读直接放弃，普通请求睡等。
            if rw_ahead {
                irq::restore_flags(flags);
                buffer::unlock_buffer(n);
                return;
            }
            (*core::ptr::addr_of_mut!(WAIT_FOR_REQUEST)).sleep_on();
            irq::restore_flags(flags);
        };

        // 先把 b_data 取出来，再填请求：`q.buffer = bh(n).b_data` 会让
        // `req(r)` 的 `&mut Request` 和 `bh(n)` 的 `&mut BufferHead` 同时活着。
        // 两条 `&mut` 都带 noalias，写的顺序与可见性交给优化器决定，
        // 结果是 `q.buffer` 有机会留成空指针或旧值——症状是
        // `copy_nonoverlapping requires ... non-null`，或者一次传输写到了
        // 上一个请求的缓冲上（随机文件系统损坏）。
        let data = bh(n).b_data;
        let q = req(r);
        q.cmd = rw;
        q.errors = 0;
        q.sector = sector;
        q.nr_sectors = count;
        q.current_nr_sectors = count;
        q.buffer = data;
        q.waiting = NIL;
        q.bh = n;
        q.bhtail = n;
        q.next = NIL;
        bh(n).b_reqnext = NIL;

        let flags = irq::local_irq_save();
        add_request(m, r);
        irq::restore_flags(flags);
    }
}

/// 提交一批块 I/O。对应原版 `ll_rw_block()`。
///
/// 与原版的签名差异：原版是 `(int rw, int nr, struct buffer_head * bh[])`，
/// 数组里可以有 NULL 洞（原版开头那个 `while (!*bh)` 就是跳洞）。我们收
/// 缓冲下标的切片，没有洞的概念，所以那段跳过逻辑不需要。
///
/// # Safety
/// 不能在中断上下文调用。切片里的下标必须都是有效缓冲且调用者持有引用。
pub unsafe fn ll_rw_block(rw: i32, bhs: &mut [usize]) {
    if bhs.is_empty() {
        return;
    }
    // SAFETY: 契约转交。
    unsafe {
        let dev = bh(bhs[0]).b_dev;
        let m = major(dev) as usize;

        let has_fn = m < MAX_BLKDEV && blk_dev(m).request_fn.is_some();
        if !has_fn {
            pr_err!(
                "ll_rw_block: Trying to read nonexistent block-device {:04x} ({})",
                dev,
                bh(bhs[0]).b_blocknr
            );
            for &b in bhs.iter() {
                let b = bh(b);
                b.b_dirt = false;
                b.b_uptodate = false;
            }
            return;
        }

        // 原版从 blksize_size[major][minor] 取正确块大小，缺省 BLOCK_SIZE。
        // 我们只支持 BLOCK_SIZE（见 buffer::set_blocksize 的注释）。
        for &b in bhs.iter() {
            if (*crate::fs::buffer::buf_ptr(b)).b_size != crate::fs::BLOCK_SIZE {
                pr_err!(
                    "ll_rw_block: only {}-char blocks implemented ({})",
                    crate::fs::BLOCK_SIZE,
                    bh(b).b_size
                );
                for &b in bhs.iter() {
                    let b = bh(b);
                    b.b_dirt = false;
                    b.b_uptodate = false;
                }
                return;
            }
        }

        if (rw == WRITE || rw == WRITEA) && is_read_only(dev) {
            pr_err!("Can't write to read-only device {:#x}", dev);
            for &b in bhs.iter() {
                let b = bh(b);
                b.b_dirt = false;
                b.b_uptodate = false;
            }
            return;
        }

        for &b in bhs.iter() {
            bh(b).b_req = true;
            make_request(m, rw, b);
        }
    }
}

// ---- 给驱动用的队首访问与完成通知（原版 blk.h 的那几个宏）----

/// 队首请求的下标，[`NIL`] 表示队列空。对应原版 `CURRENT` 宏
/// （`blk_dev[MAJOR_NR].current_request`）。
///
/// # Safety
/// 在驱动的 `request_fn` 里调用；`m < MAX_BLKDEV`。
#[inline]
pub unsafe fn current_request(m: u32) -> usize {
    // SAFETY: 契约转交。
    unsafe { blk_dev(m as usize).current_request }
}

/// 队首请求的引用。
///
/// # Safety
/// 调用前必须确认 [`current_request`] 不是 [`NIL`]。
#[inline]
pub unsafe fn cur(m: u32) -> &'static mut Request {
    // SAFETY: 契约保证队列非空。
    unsafe { req(blk_dev(m as usize).current_request) }
}

/// 队首合法性检查。对应原版 `INIT_REQUEST` 宏。
/// 返回 false 表示队列空，驱动应当直接返回。
///
/// 原版那两个 `panic`（请求链被破坏、块没上锁）照搬：它们检查的是
/// 「队首请求的主设备号必须等于本驱动的」和「有缓冲就必须已锁」，
/// 都是驱动与请求层之间的不变式，静默放过只会让错误跑得更远。
///
/// # Safety
/// 在驱动的 `request_fn` 里调用。
pub unsafe fn init_request(m: u32, name: &str) -> bool {
    // SAFETY: 契约转交。
    unsafe {
        let n = current_request(m);
        if n == NIL {
            return false;
        }
        let dev = req(n).dev.unwrap_or(0);
        if major(dev) != m {
            panic!("{}: request list destroyed", name);
        }
        let b = req(n).bh;
        if b != NIL && !bh(b).b_lock {
            panic!("{}: block not locked", name);
        }
        true
    }
}

/// 结束队首请求的当前缓冲。对应原版 `blk.h` 的 `end_request()`。
///
/// 原版语义逐条保留：
/// - `uptodate == false`（I/O 出错）时跳过整个 1024 字节块，把 `sector`
///   向上对齐到块边界，让后续缓冲还有机会成功
/// - 一条请求可以串多个缓冲（`b_reqnext`），逐个解锁并前移 `buffer`
/// - 缓冲全部处理完才把请求出队、唤醒 `waiting` 的任务、释放槽位
/// - 唤醒的任务时间片比当前多就置 `need_resched`
///
/// # Safety
/// 由驱动在 `request_fn` 或其 I/O 完成中断里调用。可在中断上下文调用
/// （只用 `unlock_buffer`/`wake_up`，都不睡）。
pub unsafe fn end_request(m: u32, uptodate: bool) {
    // SAFETY: 契约转交。
    unsafe {
        let n = current_request(m);
        if n == NIL {
            return;
        }
        req(n).errors = 0;

        if !uptodate {
            let q = req(n);
            pr_err!("dev {:04x}, sector {} I/O error", q.dev.unwrap_or(0), q.sector);
            q.nr_sectors -= 1;
            q.nr_sectors &= !super::SECTOR_MASK;
            q.sector += (crate::fs::BLOCK_SIZE / SECTOR_SIZE) as u32;
            q.sector &= !super::SECTOR_MASK;
        }

        let b = req(n).bh;
        if b != NIL {
            let next_b = bh(b).b_reqnext;
            req(n).bh = next_b;
            bh(b).b_reqnext = NIL;
            bh(b).b_uptodate = uptodate;
            buffer::unlock_buffer(b);
            if next_b != NIL {
                // 还有后续缓冲：把请求的游标挪过去，本次调用结束。
                // 同 make_request：先从缓冲头把值取出来，再动请求，
                // 别让 req()/bh() 的两条 &mut 同时活着。
                let (nsz, ndata) = { let p = bh(next_b); (p.b_size, p.b_data) };
                let q = req(n);
                q.current_nr_sectors = (nsz / SECTOR_SIZE) as u32;
                if q.nr_sectors < q.current_nr_sectors {
                    q.nr_sectors = q.current_nr_sectors;
                    pr_err!("end_request: buffer-list destroyed");
                }
                q.buffer = ndata;
                return;
            }
        }

        // 这条请求彻底做完了：出队 + 唤醒等待者 + 释放槽位
        // 同上：先取 next，再写 blk_dev，避免 blk_dev()/req() 的两条 &mut 重叠。
        let nx = req(n).next;
        blk_dev(m as usize).current_request = nx;
        let w = req(n).waiting;
        if w != NIL {
            req(n).waiting = NIL;
            buffer::wake_io_waiter(w);
        }
        req(n).dev = None;
        (*core::ptr::addr_of_mut!(WAIT_FOR_REQUEST)).wake_up();
    }
}

/// 清空请求池与所有队列。对应原版 `blk_dev_init()` 里那个
/// `for (i=0 ; i<NR_REQUEST ; i++) { all_requests[i].dev = -1; ... }`。
///
/// # Safety
/// 启动期调用一次，此时没有任何 I/O 在飞。
pub unsafe fn init() {
    // SAFETY: 契约保证独占。
    unsafe {
        for i in 0..NR_REQUEST {
            *req(i) = Request::new();
        }
        for m in 0..MAX_BLKDEV {
            let d = blk_dev(m);
            d.request_fn = None;
            d.current_request = NIL;
        }
    }
}

/// 队列统计。原版没有，自检用。
pub fn queue_depth(m: u32) -> usize {
    if m as usize >= MAX_BLKDEV {
        return 0;
    }
    // SAFETY: 已查界；只读链表，关中断防止驱动在中途改链。
    unsafe {
        let flags = irq::local_irq_save();
        let mut n = blk_dev(m as usize).current_request;
        let mut d = 0;
        while n != NIL && d <= NR_REQUEST {
            d += 1;
            n = req(n).next;
        }
        irq::restore_flags(flags);
        d
    }
}
