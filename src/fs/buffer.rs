//! 缓冲区高速缓存。对应 linux-1.0.9 的 `fs/buffer.c` 与 `fs.h` 里的
//! `struct buffer_head`。
//!
//! # 与原版的结构性差异
//!
//! 1. **裸指针链 → 下标链**（见 `fs/mod.rs` 模块文档第 1 点）。原版每个
//!    `buffer_head` 有六个指针字段（`b_prev`/`b_next` 哈希链、
//!    `b_prev_free`/`b_next_free` 空闲环、`b_this_page`、`b_reqnext`），
//!    我们全部换成 `usize` 下标，[`NIL`] 表示空。
//! 2. **缓冲头静态预分配**。原版 `buffer_head` 本身也是动态申请的
//!    （`get_more_buffer_heads` 从 `get_free_page` 切出 `PAGE_SIZE/sizeof`
//!    个头挂到 `unused_list`），并且 `grow_buffers`/`shrink_buffers` 会随
//!    内存压力伸缩缓存。我们固定 [`NR_BUFFERS`] 个头，数据页在 `init` 里
//!    一次性分配。这样就不需要 `unused_list`、`grow_buffers`、
//!    `shrink_buffers`、`try_to_free`，也不需要 `b_this_page`
//!    （原版靠它在释放整页时找到同页的兄弟缓冲）。
//!    代价是缓存容量固定，`getblk` 找不到可用缓冲时只能睡等而不能扩容。
//! 3. **不移植 `bread_page`/`breada`/`check_aligned`/`try_to_share_buffers`**：
//!    这几个是给 `mm/memory.c` 的按页读文件与 buffer/page cache 共享做的，
//!    依赖尚未移植的 `mmap`。
//! 4. 原版 `set_blocksize` 支持 512/1024/2048/4096 四种块大小。我们只支持
//!    [`BLOCK_SIZE`] = 1024（minix 和 ramdisk 都只用这个），`set_blocksize`
//!    仍然保留但只接受 1024，理由见该函数注释。

use crate::{pr_info, pr_warn};
use crate::drivers::block::ll_rw_block;
use crate::fs::{READ, WRITE, major, minor};
use crate::irq;
use crate::mm::page::PAGE_SIZE;
use crate::mm::page_alloc;
use crate::sched::{TaskState, WaitQueue, current_nr, schedule, task};

/// 块大小。对应原版 `fs.h` 的 `BLOCK_SIZE 1024`。
pub const BLOCK_SIZE: usize = 1024;
/// 对应原版 `BLOCK_SIZE_BITS 10`。
pub const BLOCK_SIZE_BITS: usize = 10;

/// 缓冲区个数。原版没有这个常量（缓存大小随内存动态伸缩，
/// 见模块文档第 2 点）。64 个 1024 字节缓冲 = 64KB = 16 页。
pub const NR_BUFFERS: usize = 64;

/// 哈希桶数。原版 `fs.h` 的 `NR_HASH 997`；缓冲总数只有 64，取 61
/// （素数，同原版取素数的用意：`(dev^block) % NR_HASH` 分布均匀）。
pub const NR_HASH: usize = 61;

/// 空下标。原版对应的是指针 `NULL`。
pub const NIL: usize = usize::MAX;

/// 缓冲头。对应原版 `struct buffer_head`。
///
/// 原版字段里 `b_uptodate`/`b_dirt`/`b_lock`/`b_req` 是 `unsigned char`
/// 当布尔用，这里直接用 `bool`。
pub struct BufferHead {
    /// 数据块地址（1024 字节）。原版 `char * b_data`
    pub b_data: *mut u8,
    /// 块大小。原版 `unsigned long b_size`
    pub b_size: usize,
    /// 块号。原版 `unsigned long b_blocknr`
    pub b_blocknr: u32,
    /// 所属设备，0 表示这个缓冲还没绑定设备。原版 `dev_t b_dev`
    pub b_dev: u16,
    /// 引用计数。原版 `unsigned short b_count`
    pub b_count: u16,
    /// 内容与磁盘一致。原版 `b_uptodate`
    pub b_uptodate: bool,
    /// 内容比磁盘新，需要回写。原版 `b_dirt`
    pub b_dirt: bool,
    /// I/O 进行中。原版 `b_lock`
    pub b_lock: bool,
    /// 曾经发起过 I/O（原版靠它区分「读失败」和「从没读过」）。原版 `b_req`
    pub b_req: bool,
    /// 等 `b_lock` 放开的任务。原版 `struct wait_queue * b_wait`
    pub b_wait: WaitQueue,

    /// 哈希链前驱。原版 `b_prev`
    pub b_prev: usize,
    /// 哈希链后继。原版 `b_next`
    pub b_next: usize,
    /// 空闲环前驱。原版 `b_prev_free`
    pub b_prev_free: usize,
    /// 空闲环后继。原版 `b_next_free`
    pub b_next_free: usize,
    /// 请求队列里的下一个缓冲。原版 `b_reqnext`
    pub b_reqnext: usize,
}

impl BufferHead {
    const fn new() -> Self {
        BufferHead {
            b_data: core::ptr::null_mut(),
            b_size: 0,
            b_blocknr: 0,
            b_dev: 0,
            b_count: 0,
            b_uptodate: false,
            b_dirt: false,
            b_lock: false,
            b_req: false,
            b_wait: WaitQueue::new(),
            b_prev: NIL,
            b_next: NIL,
            b_prev_free: NIL,
            b_next_free: NIL,
            b_reqnext: NIL,
        }
    }

    /// 数据区是否可以安全地做成切片。`b_data` 必须非空、落在缓冲缓存
    /// 分配数据页的那段物理内存里、且 `b_size` 恰为 [`BLOCK_SIZE`]。
    ///
    /// 只查空指针不够：`b_data` 被写坏成一个非零垃圾值时，
    /// `from_raw_parts` 的对齐/长度前置条件会以
    /// `hint::assert_unchecked` 或 `slice::iter` 里的泛化 UB 消息形式炸掉，
    /// 完全看不出是哪个缓冲头坏了。
    fn data_ok(&self) -> bool {
        let d = self.b_data as usize;
        self.b_size == BLOCK_SIZE
            && d >= DATA_MIN
            && d % core::mem::align_of::<u64>() == 0
            && d.checked_add(BLOCK_SIZE)
                .is_some_and(|e| e <= crate::mm::page_alloc::high_memory())
    }

    /// 数据区的只读切片。
    ///
    /// # Safety
    /// 调用者必须持有该缓冲的引用（`b_count > 0`）且当前没有 I/O 在进行
    /// （`b_lock == false`），否则读到的可能是半截数据。
    pub unsafe fn data(&self) -> &[u8] {
        // b_data 为空说明这个缓冲头还没被 init 挂上数据页，却已经被当成
        // 有效缓冲用了。空指针交给 from_raw_parts 是 UB（表现为
        // slice/iter.rs 里的 assert_unchecked 失败，且看不出是谁的错），
        // 所以在这里挡一刀，报出确定性的信息。
        // 完整校验而不只查空指针：`from_raw_parts` 的前置条件里还有对齐和
        // 长度，b_size 被写坏时报出来的是那条泛化的 UB 消息，看不出是哪个
        // 缓冲头坏了。
        if !self.data_ok() {
            panic!("buffer: data() on bad buffer (idx={} b_data={:#x} b_size={} \
                    b_dev={:#06x} b_blocknr={} count={} lock={})",
                   index_of(self), self.b_data as usize, self.b_size,
                   self.b_dev, self.b_blocknr, self.b_count, self.b_lock);
        }
        // SAFETY: b_data 由 init 从页分配器取得，长度恰为 b_size；
        // 契约保证没有并发 I/O 在改它。
        unsafe { core::slice::from_raw_parts(self.b_data, self.b_size) }
    }

    /// 数据区的可写切片。改完必须置 `b_dirt = true`。
    ///
    /// # Safety
    /// 同 [`data`](Self::data)。
    pub unsafe fn data_mut(&mut self) -> &mut [u8] {
        // 见 [`data`](Self::data) 里的说明。
        if !self.data_ok() {
            panic!("buffer: data_mut() on bad buffer (idx={} b_data={:#x} b_size={} \
                    b_dev={:#06x} b_blocknr={} count={} lock={})",
                   index_of(self), self.b_data as usize, self.b_size,
                   self.b_dev, self.b_blocknr, self.b_count, self.b_lock);
        }
        // SAFETY: 同 data；&mut self 保证 Rust 侧独占。
        unsafe { core::slice::from_raw_parts_mut(self.b_data, self.b_size) }
    }
}

/// 自检：同一个 (dev, block) 是否存在两个缓冲。缓存的核心不变量就是
/// 「一块一缓冲」——破了它，写会落到一个副本上而读从另一个副本来，
/// 表现为随机丢失的写。原版没有这个检查（它信任 hash queue）。
///
/// 返回重复的块号个数。
///
/// # Safety
/// 只能在进程上下文调用；只读缓冲头。
pub unsafe fn check_duplicates() -> usize {
    // SAFETY: 契约转交。
    unsafe {
        let mut dups = 0;
        for i in 0..NR_BUFFERS {
            let (di, bi, ci) = { let p = bh(i); (p.b_dev, p.b_blocknr, p.b_count) };
            if di == 0 {
                continue;
            }
            for j in (i + 1)..NR_BUFFERS {
                let (dj, bj) = { let p = bh(j); (p.b_dev, p.b_blocknr) };
                if dj == di && bj == bi {
                    crate::pr_err!(
                        "buffer: DUPLICATE dev={:#06x} block={} bufs {}(count={}) and {}(count={})",
                        di, bi, i, ci, j, bh(j).b_count
                    );
                    dups += 1;
                }
            }
        }
        dups
    }
}

/// 数据页的合法范围下界。见 [`init`] 里的断言。
const DATA_MIN: usize = 0x10_0000;

/// 全表体检：每个缓冲头的 `b_size`/`b_data` 和四条链的下标都必须自洽。
///
/// 原版没有这个。加它的原因是观察到「`b_size` 变成 0 的缓冲头出现在哈希链
/// 上」和「缓冲数据里出现 BIOS ROM 的 `0xf000ff53`」——两者都说明
/// `BUFFERS` 这块 BSS 被别人写了，而不是缓冲层自己的逻辑错。逐个检查能
/// 区分「一个头坏了」（逻辑 bug）和「一片头坏了」（内存被覆盖）。
///
/// 返回坏掉的头数，并把前几个的详情打出来。
///
/// # Safety
/// 只读缓冲头表。可在任何上下文调用。
pub unsafe fn verify(tag: &str) -> usize {
    // SAFETY: 契约转交。
    unsafe {
        let built = *core::ptr::addr_of!(NR_BUFFERS_USED);
        let hi = crate::mm::page_alloc::high_memory();
        let mut bad = 0;
        for n in 0..built {
            let b = &(*core::ptr::addr_of!(BUFFERS))[n];
            let d = b.b_data as usize;
            let size_ok = b.b_size == BLOCK_SIZE;
            let data_ok = d >= DATA_MIN && d + BLOCK_SIZE <= hi;
            let link_ok = (b.b_next == NIL || b.b_next < NR_BUFFERS)
                && (b.b_prev == NIL || b.b_prev < NR_BUFFERS)
                && (b.b_next_free == NIL || b.b_next_free < NR_BUFFERS)
                && (b.b_prev_free == NIL || b.b_prev_free < NR_BUFFERS);
            if size_ok && data_ok && link_ok {
                continue;
            }
            bad += 1;
            if bad <= 4 {
                crate::pr_err!(
                    "buffer::verify[{}]: head {} corrupt: b_size={} b_data={:#x} \
                     dev={:#06x} blk={} next={} prev={} nf={} pf={}",
                    tag, n, b.b_size, d, b.b_dev, b.b_blocknr,
                    b.b_next, b.b_prev, b.b_next_free, b.b_prev_free
                );
            }
        }
        if bad != 0 {
            crate::pr_err!("buffer::verify[{}]: {} of {} heads corrupt", tag, bad, built);
        }
        bad
    }
}

/// 某个物理页是否被缓冲缓存的数据区占用。给 ramdisk 自查重叠用
/// （见 `drivers::block::ramdisk::init`）。原版没有。
pub fn owns_page(page: usize) -> bool {
    // SAFETY: 只读缓冲头表。
    unsafe {
        let built = *core::ptr::addr_of!(NR_BUFFERS_USED);
        for n in 0..built {
            let d = (*core::ptr::addr_of!(BUFFERS))[n].b_data as usize;
            if d != 0 && d & !(crate::mm::PAGE_SIZE - 1) == page {
                return true;
            }
        }
        false
    }
}

/// 缓冲头表。原版是动态链表，见模块文档第 2 点。
static mut BUFFERS: [BufferHead; NR_BUFFERS] = [const { BufferHead::new() }; NR_BUFFERS];

/// 哈希表。对应原版 `static struct buffer_head * hash_table[NR_HASH]`。
static mut HASH_TABLE: [usize; NR_HASH] = [NIL; NR_HASH];

/// 空闲环的表头。对应原版 `static struct buffer_head * free_list`。
/// 注意原版这是个**环形**双向链表，`free_list` 指向「最久未用」的一端，
/// 新释放的缓冲挂到 `free_list->b_prev_free`（即环的末尾）。
static mut FREE_LIST: usize = NIL;

/// 等空闲缓冲的任务。对应原版 `static struct wait_queue * buffer_wait`。
static mut BUFFER_WAIT: WaitQueue = WaitQueue::new();

/// 缓冲总数。对应原版 `int nr_buffers`。
static mut NR_BUFFERS_USED: usize = 0;

/// 取缓冲头。
///
/// # Safety
/// `nr` 必须 `< NR_BUFFERS`。调用者需保证不与其他 `&mut` 别名同时存在。
#[inline]
#[track_caller]
pub unsafe fn bh(nr: usize) -> &'static mut BufferHead {
    // 下标越界立刻报出来。这里最常见的错因是沿 b_next_free/b_next 走到了
    // 一个被写坏的链接值；不挡的话下一步就是拿垃圾指针构造切片。
    assert!(nr < NR_BUFFERS, "bh(): index {} out of range", nr);
    // SAFETY: 上面已校验下标在界内；单核内核，调用点自行保证不重叠借用。
    unsafe { &mut (*core::ptr::addr_of_mut!(BUFFERS))[nr] }
}

/// 缓冲头的裸指针。
///
/// 链表操作**必须**用这个而不是 [`bh`]。原因：`bh` 返回
/// `&'static mut BufferHead`，一条语句里取两次（`bh(tail)` 与 `bh(head)`，
/// 而环短的时候 `tail == head`；或者 `bh(n)` 与 `bh(prev)` 在 `n == prev`
/// 时）就构成两条指向同一对象的可变引用。Rust 的 `&mut` 带 `noalias`
/// 语义，LLVM 因此可以假定通过其中一条写入不影响另一条，于是把后一次
/// 写合并/丢弃掉——结果是环上留下一个只连了一半的节点
/// （`b_next_free` 有值而对侧的 `b_prev_free` 还是 `NIL`）。
///
/// 这正是「约 15% 概率的随机 fs 损坏」的根因：坏掉的环会让 `getblk` 走到
/// `NIL` 下标，或者让同一个缓冲同时被当成两个块用，症状五花八门
/// （`buf 0 bytes`、缓冲里出现 BIOS ROM 字节、mount 找不到魔数、漏一个
/// zone），且都离原因很远。裸指针的 `(*p).field = v` 是 place 写，不产生
/// 引用、不带 `noalias`，与原版 C 的指针语义一致。
///
/// # Safety
/// `n < NR_BUFFERS`（内部断言）；调用者负责不与并发写交错（关中断或
/// 进程上下文独占）。
#[inline]
#[track_caller]
pub unsafe fn buf_ptr(n: usize) -> *mut BufferHead {
    assert!(n < NR_BUFFERS, "buf_ptr(): index {} out of range", n);
    // SAFETY: 下标已校验；BUFFERS 是地址恒定的静态数组。
    unsafe { (*core::ptr::addr_of_mut!(BUFFERS)).as_mut_ptr().add(n) }
}

/// 缓冲头在表里的下标。给驱动层用（它拿到的是 `&BufferHead`）。
pub fn index_of(b: &BufferHead) -> usize {
    let base = core::ptr::addr_of!(BUFFERS) as usize;
    (b as *const BufferHead as usize - base) / core::mem::size_of::<BufferHead>()
}

/// 哈希函数。对应原版 `_hashfn(dev,block) (((unsigned)(dev^block))%NR_HASH)`。
#[inline]
fn hashfn(dev: u16, block: u32) -> usize {
    ((dev as u32 ^ block) as usize) % NR_HASH
}

// ---- 链表操作。逐一对应原版那五个 `static inline` ----

/// 走一遍空闲环，确认它是个恰好含 `NR_BUFFERS_USED` 个节点的双向环，
/// 且 `FREE_LIST` 在环上。`tag` 标出调用点。
///
/// 原版没有这个。留着它是因为环一旦半连（`b_next_free` 有值而对侧的
/// `b_prev_free` 还是 `NIL`），后果要到很远的地方才显形——`getblk` 沿链
/// 走到 `NIL` 下标、或者同一个缓冲被当成两个块用，症状是随机的文件系统
/// 损坏。这个检查能把失败点钉在破环的那次操作上。
///
/// 只在 debug 构建里跑（`debug_assertions`）：release 下是空函数。
///
/// # Safety
/// 只读缓冲头表。
#[inline]
unsafe fn check_free_ring(tag: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    // SAFETY: 只读。
    unsafe {
        let built = *core::ptr::addr_of!(NR_BUFFERS_USED);
        if built == 0 {
            return;
        }
        let head = *core::ptr::addr_of!(FREE_LIST);
        if head == NIL {
            crate::pr_err!("buffer: free ring[{}]: FREE_LIST is NIL", tag);
            panic!("free ring empty at {}", tag);
        }
        let hp = (*core::ptr::addr_of!(BUFFERS))[head].b_prev_free;
        if hp == NIL || hp >= NR_BUFFERS {
            crate::pr_err!("buffer: free ring[{}]: FREE_LIST={} off ring (prev_free={})", tag, head, hp);
            panic!("free ring head off ring at {}", tag);
        }
        let mut p = head;
        let mut cnt = 0usize;
        loop {
            let b = &(*core::ptr::addr_of!(BUFFERS))[p];
            let nx = b.b_next_free;
            if nx == NIL || nx >= NR_BUFFERS {
                crate::pr_err!("buffer: free ring[{}]: node {} next_free={}", tag, p, nx);
                panic!("free ring broken at {}", tag);
            }
            if (*core::ptr::addr_of!(BUFFERS))[nx].b_prev_free != p {
                crate::pr_err!("buffer: free ring[{}]: node {} next={} but its prev={}",
                    tag, p, nx, (*core::ptr::addr_of!(BUFFERS))[nx].b_prev_free);
                panic!("free ring asymmetric at {}", tag);
            }
            cnt += 1;
            p = nx;
            if p == head {
                break;
            }
            if cnt > NR_BUFFERS {
                crate::pr_err!("buffer: free ring[{}]: no cycle back to head after {}", tag, cnt);
                panic!("free ring not circular at {}", tag);
            }
        }
        // 环上节点数：`insert_at_free_tail` 进来时那个待插入的节点已经被
        // `remove_from_free_list` 摘下来了，所以允许少一个。少两个以上说明
        // 真的漏了节点。
        if cnt + 1 < built {
            crate::pr_err!("buffer: free ring[{}]: {} nodes on ring, expected {}", tag, cnt, built);
            panic!("free ring wrong length at {}", tag);
        }
    }
}

/// 对应原版 `remove_from_hash_queue()`。
///
/// # Safety
/// `n < NR_BUFFERS`；调用者已关中断或确保无并发。
unsafe fn remove_from_hash_queue(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        // 全程用裸指针写字段，见 [`buf_ptr`] 的说明（n/prev/next 可能相等）。
        let pn = buf_ptr(n);
        let (prev, next, dev, blk) =
            ((*pn).b_prev, (*pn).b_next, (*pn).b_dev, (*pn).b_blocknr);
        if next != NIL {
            (*buf_ptr(next)).b_prev = prev;
        }
        if prev != NIL {
            (*buf_ptr(prev)).b_next = next;
        }
        let head = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(HASH_TABLE))[hashfn(dev, blk)]);
        if *head == n {
            *head = next;
        }
        (*pn).b_prev = NIL;
        (*pn).b_next = NIL;
    }
}

/// 对应原版 `remove_from_free_list()`。原版在链断了的时候
/// `panic("VFS: Free block list corrupted")`，这里照搬。
///
/// # Safety
/// 同 [`remove_from_hash_queue`]。
unsafe fn remove_from_free_list(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        check_free_ring("remove_from_free_list");
        // 裸指针写：n/prev/next 三者可能两两相等（环上只剩一两个节点时），
        // 用 `bh()` 取多份 `&mut` 会因 noalias 丢写。见 [`buf_ptr`]。
        let pn = buf_ptr(n);
        let (prev, next) = ((*pn).b_prev_free, (*pn).b_next_free);
        if prev == NIL || next == NIL {
            panic!("VFS: Free block list corrupted");
        }
        (*buf_ptr(prev)).b_next_free = next;
        (*buf_ptr(next)).b_prev_free = prev;
        let free_list = core::ptr::addr_of_mut!(FREE_LIST);
        if *free_list == n {
            *free_list = next;
        }
        (*pn).b_next_free = NIL;
        (*pn).b_prev_free = NIL;
    }
}

/// 对应原版 `remove_from_queues()`。
///
/// # Safety
/// 同 [`remove_from_hash_queue`]。
unsafe fn remove_from_queues(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        remove_from_hash_queue(n);
        remove_from_free_list(n);
    }
}

/// 把缓冲挂到空闲环末尾（即 `free_list` 的前驱）。
/// 对应原版 `insert_into_queues()` 的前半段那四行。
///
/// # Safety
/// 同 [`remove_from_hash_queue`]；且 `FREE_LIST` 非空。
unsafe fn insert_at_free_tail(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let head = *core::ptr::addr_of!(FREE_LIST);
        // `tail == head` 在环上只有一个节点时成立，`tail == n` 也可能；
        // 必须用裸指针逐个字段写，见 [`buf_ptr`]。原版这四行是
        //   bh->b_next_free = free_list;
        //   bh->b_prev_free = free_list->b_prev_free;
        //   free_list->b_prev_free->b_next_free = bh;
        //   free_list->b_prev_free = bh;
        // 顺序照搬（第 4 行必须在第 3 行之后：第 3 行还要用旧的 tail）。
        check_free_ring("insert_at_free_tail");
        let tail = (*buf_ptr(head)).b_prev_free;
        let pn = buf_ptr(n);
        (*pn).b_next_free = head;
        (*pn).b_prev_free = tail;
        (*buf_ptr(tail)).b_next_free = n;
        (*buf_ptr(head)).b_prev_free = n;
    }
}

/// 对应原版 `put_last_free()`：把缓冲移到「最近使用」的一端。
/// 原版对 `bh == free_list` 的特殊处理（直接把表头往后挪一格）也照搬——
/// 因为环里 `free_list` 本身就是末尾的下一个，把表头前移等价于把它变成末尾。
///
/// # Safety
/// 同 [`remove_from_hash_queue`]。
unsafe fn put_last_free(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let free_list = core::ptr::addr_of_mut!(FREE_LIST);
        if *free_list == n {
            *free_list = (*buf_ptr(n)).b_next_free;
            return;
        }
        remove_from_free_list(n);
        insert_at_free_tail(n);
    }
}

/// 对应原版 `insert_into_queues()`。
///
/// # Safety
/// 同 [`remove_from_hash_queue`]。
unsafe fn insert_into_queues(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        insert_at_free_tail(n);
        // 原版：没有设备的缓冲不进哈希表。裸指针写，见 [`buf_ptr`]
        // （`old == n` 不该发生，但真发生时丢写比 panic 更难查）。
        let pn = buf_ptr(n);
        (*pn).b_prev = NIL;
        (*pn).b_next = NIL;
        let (dev, blk) = ((*pn).b_dev, (*pn).b_blocknr);
        if dev == 0 {
            return;
        }
        let head = core::ptr::addr_of_mut!((*core::ptr::addr_of_mut!(HASH_TABLE))[hashfn(dev, blk)]);
        let old = *head;
        (*pn).b_next = old;
        *head = n;
        if old != NIL {
            (*buf_ptr(old)).b_prev = n;
        }
    }
}

/// 对应原版 `find_buffer()`。块大小不符时原版打印警告并返回 NULL，照搬。
///
/// # Safety
/// 调用者已关中断或确保无并发改哈希链。
unsafe fn find_buffer(dev: u16, block: u32, size: usize) -> Option<usize> {
    // SAFETY: 契约转交；只读哈希链。
    unsafe {
        let mut p = (*core::ptr::addr_of!(HASH_TABLE))[hashfn(dev, block)];
        while p != NIL {
            let b = bh(p);
            if b.b_dev == dev && b.b_blocknr == block {
                if b.b_size == size {
                    return Some(p);
                }
                pr_warn!("VFS: Wrong blocksize on device {}/{}", major(dev), minor(dev));
                return None;
            }
            p = b.b_next;
        }
        None
    }
}

/// 等 `b_lock` 放开。对应原版 `wait_on_buffer()` / `__wait_on_buffer()`。
///
/// 原版把节点放在调用者栈上（`struct wait_queue wait = { current, NULL }`），
/// 我们的 [`WaitQueue`] 是侵入式的，所以直接 `sleep_on`。原版在睡之前先
/// `b_count++`、醒来后 `b_count--`，防止缓冲在睡眠期间被复用；照搬。
///
/// # Safety
/// 不能在中断上下文或 task[0] 里调用（会 `sleep_on`）。
pub unsafe fn wait_on_buffer(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        if !bh(n).b_lock {
            return;
        }
        (*buf_ptr(n)).b_count += 1;
        // 原版 __wait_on_buffer 用 TASK_UNINTERRUPTIBLE：I/O 完成前不能被
        // 信号打断，否则缓冲会在 DMA 中途被复用。
        //
        // 必须用 sleep_on_while 而不是「while b_lock { sleep_on() }」：
        // 后者在判完 b_lock 到挂上等待队列之间有窗口，end_request 在中断
        // 里的 wake_up 落进这个窗口就丢了，这个任务永久睡死、b_count 再也
        // 回不到 1。见 sleep_on_while 的文档。
        // 用 sleep_on_while 而不是「while b_lock { sleep_on() }」：后者在判完
        // b_lock 到挂上等待队列之间有窗口，end_request 在中断里的 wake_up
        // 落进这个窗口就丢了，任务永久睡死、b_count 再也回不到 1
        // （症状：minix_truncate 一直 retry 到放弃，漏掉那个块）。
        //
        // 注意这里**不能**把 `&mut b_wait` 和读 `b_lock` 的闭包同时通过
        // `bh()` 取——那是对同一个 static mut 的两条重叠可变借用路径，
        // 优化后会读到过期的 b_lock。改成先算出两个裸指针再用。
        let bp = buf_ptr(n);
        let wq = core::ptr::addr_of_mut!((*bp).b_wait);
        let lock = core::ptr::addr_of!((*bp).b_lock);
        (*wq).sleep_on_while(|| core::ptr::read_volatile(lock));
        (*buf_ptr(n)).b_count -= 1;
    }
}

/// 加锁。对应原版 `include/linux/locks.h` 的 `lock_buffer()`。
/// 原版是 `wait_on_buffer(bh); bh->b_lock = 1;`。
///
/// # Safety
/// 同 [`wait_on_buffer`]。
pub unsafe fn lock_buffer(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        wait_on_buffer(n);
        bh(n).b_lock = true;
    }
}

/// 解锁并唤醒等待者。对应原版 `unlock_buffer()`。
///
/// # Safety
/// `n < NR_BUFFERS`。可在中断上下文调用（`end_request` 会调），
/// 因为 `wake_up` 本身只改任务状态、不睡。
pub unsafe fn unlock_buffer(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let b = bh(n);
        b.b_lock = false;
        b.b_wait.wake_up();
    }
}

// ---- getblk / brelse / bread ----

/// 查哈希表并取一个引用。对应原版 `get_hash_table()`。
///
/// 原版那个 `for(;;)` 循环不是多余的：`wait_on_buffer` 会睡，睡醒后这个
/// 缓冲可能已经被别人拿去装别的块了，所以要重新校验 dev/block/size。照搬。
///
/// # Safety
/// 不能在中断上下文调用（会睡）。
pub unsafe fn get_hash_table(dev: u16, block: u32, size: usize) -> Option<usize> {
    // SAFETY: 契约转交。
    unsafe {
        loop {
            let n = find_buffer(dev, block, size)?;
            (*buf_ptr(n)).b_count += 1;
            wait_on_buffer(n);
            // wait_on_buffer 里也会取这个缓冲的 &mut，所以这里同样用裸指针
            let bp = buf_ptr(n);
            if (*bp).b_dev == dev && (*bp).b_blocknr == block && (*bp).b_size == size {
                return Some(n);
            }
            (*bp).b_count -= 1;
        }
    }
}

/// 「坏度」评分，用来在空闲环里挑一个最不心疼的缓冲。
/// 对应原版 `#define BADNESS(bh) (((bh)->b_dirt<<1)+(bh)->b_lock)`：
/// 脏比锁贵一倍，0 分（干净且空闲）最理想。
#[inline]
unsafe fn badness(n: usize) -> u32 {
    // SAFETY: 契约同 bh()。
    let b = unsafe { bh(n) };
    ((b.b_dirt as u32) << 1) + b.b_lock as u32
}

/// 取一个装着 (dev, block) 的缓冲，内容不保证有效。
/// 对应原版 `getblk()`。
///
/// 与原版的差异：原版找不到可用缓冲时调 `grow_buffers` 扩容，
/// 我们的缓存是定长的（模块文档第 2 点），所以只能 `sleep_on(&buffer_wait)`
/// 等别人 `brelse`。`shrink_buffers`/`grow_buffers` 那两段随之删掉。
///
/// # Safety
/// 不能在中断上下文或 task[0] 里调用（会睡）。
pub unsafe fn getblk(dev: u16, block: u32, size: usize) -> Option<usize> {
    // SAFETY: 契约转交。
    unsafe {
        loop {
            // 原版 repeat: 标签
            if let Some(n) = get_hash_table(dev, block, size) {
                // get_hash_table 已按 size 匹配过，这里再确认一次数据区
                // 真的建起来了：b_size==0 的缓冲头意味着它从没经过 init，
                // 却已经挂进了哈希链——那是链表被写坏的信号。
                debug_assert_eq!(bh(n).b_size, size, "getblk: hash hit with wrong b_size");
                // 原版：命中且干净就顺手把它移到「最近使用」端
                let (up, dirt) = { let p = bh(n); (p.b_uptodate, p.b_dirt) };
                if up && !dirt {
                    put_last_free(n);
                }
                return Some(n);
            }

            // 沿空闲环挑一个 BADNESS 最小的。原版扫 nr_buffers 个。
            let mut best = NIL;
            let mut p = *core::ptr::addr_of!(FREE_LIST);
            for _ in 0..*core::ptr::addr_of!(NR_BUFFERS_USED) {
                if p == NIL {
                    break;
                }
                let ok = {
                    let bp = buf_ptr(p);
                    (*bp).b_count == 0 && (*bp).b_size == size
                };
                if ok && (best == NIL || badness(p) < badness(best)) {
                    best = p;
                    if badness(p) == 0 {
                        break;
                    }
                }
                p = (*buf_ptr(p)).b_next_free;
                // 空闲环的下标必须始终在界内。越界说明摘链/挂链的某条
                // 路径写坏了 b_next_free/b_prev_free；不挡的话下一轮
                // bh(p) 就会返回一个从没 init 过的缓冲头（b_size=0、
                // b_data=null），症状是「data() 返回 0 字节切片」这种
                // 完全看不出源头的断言失败。
                assert!(
                    p == NIL || p < NR_BUFFERS,
                    "buffer: free list corrupt, next_free={}",
                    p
                );
            }

            if best == NIL {
                // 原版这里 grow_buffers，扩不出来才睡。我们只能睡。
                (*core::ptr::addr_of_mut!(BUFFER_WAIT)).sleep_on();
                continue;
            }

            wait_on_buffer(best);
            // 睡醒后重新校验（原版三个 goto repeat）。裸指针：wait_on_buffer
            // 内部也取过这个缓冲的 &mut。
            let bp = buf_ptr(best);
            if (*bp).b_count != 0 || (*bp).b_size != size {
                continue;
            }
            if (*bp).b_dirt {
                sync_buffers(0, false);
                continue;
            }
            // 睡的时候别人可能已经把这个块读进缓存了
            if find_buffer(dev, block, size).is_some() {
                continue;
            }

            // 这下确定它是独一份、没人用、没锁、干净的
            (*bp).b_count = 1;
            (*bp).b_dirt = false;
            (*bp).b_uptodate = false;
            (*bp).b_req = false;
            remove_from_queues(best);
            (*bp).b_dev = dev;
            (*bp).b_blocknr = block;
            insert_into_queues(best);
            return Some(best);
        }
    }
}

/// 归还一个引用。对应原版 `brelse()`。
///
/// # Safety
/// 不能在中断上下文调用（`wait_on_buffer` 会睡）。`n` 必须是
/// [`getblk`]/[`bread`] 返回过的下标。
pub unsafe fn brelse(n: usize) {
    if n == NIL {
        return;
    }
    // SAFETY: 契约转交。
    unsafe {
        wait_on_buffer(n);
        let bp = buf_ptr(n);
        if (*bp).b_count == 0 {
            // 原版同样只是打印，不 panic
            pr_warn!("VFS: brelse: Trying to free free buffer");
            return;
        }
        (*bp).b_count -= 1;
        if (*bp).b_count == 0 {
            (*core::ptr::addr_of_mut!(BUFFER_WAIT)).wake_up();
        }
    }
}

/// 读一个块，返回内容有效的缓冲。对应原版 `bread()`。
///
/// # Safety
/// 不能在中断上下文或 task[0] 里调用。
pub unsafe fn bread(dev: u16, block: u32, size: usize) -> Option<usize> {
    // SAFETY: 契约转交。
    unsafe {
        let n = match getblk(dev, block, size) {
            Some(n) => n,
            None => {
                pr_warn!("VFS: bread: READ error on device {}/{}", major(dev), minor(dev));
                return None;
            }
        };
        if bh(n).b_uptodate {
            return Some(n);
        }
        ll_rw_block(READ, &mut [n]);
        wait_on_buffer(n);
        if bh(n).b_uptodate {
            return Some(n);
        }
        brelse(n);
        None
    }
}

/// 标脏。原版没有这个函数（调用方直接 `bh->b_dirt = 1`），
/// 但下标化之后每处都写 `bh(n).b_dirt = true` 太吵，包一层。
///
/// # Safety
/// `n < NR_BUFFERS` 且调用者持有引用。
#[inline]
pub unsafe fn mark_buffer_dirty(n: usize) {
    // SAFETY: 契约转交。
    unsafe { bh(n).b_dirt = true }
}

// ---- 回写与失效 ----

/// 回写脏缓冲。对应原版 `sync_buffers()`。
///
/// `dev == 0` 表示所有设备（同原版）。`wait` 为真时走原版那三趟：
/// 0) 写出所有干净可写的；1) 写出所有脏的（遇锁就等）；2) 只等全部解锁。
///
/// 返回是否遇到 I/O 错误（原版的 `err`）。
///
/// # Safety
/// 不能在中断上下文调用。
pub unsafe fn sync_buffers(dev: u16, wait: bool) -> bool {
    let mut err = false;
    let mut pass = 0;
    // SAFETY: 契约转交。
    unsafe {
        loop {
            let mut retry = false;
            let mut p = *core::ptr::addr_of!(FREE_LIST);
            for _ in 0..*core::ptr::addr_of!(NR_BUFFERS_USED) {
                if p == NIL {
                    break;
                }
                let next = (*buf_ptr(p)).b_next_free;
                let this = p;
                p = next;

                if dev != 0 && (*buf_ptr(this)).b_dev != dev {
                    continue;
                }
                if (*buf_ptr(this)).b_lock {
                    // 原版：不等就跳过并要求重来；等的话只在 pass>0 时真等
                    if !wait || pass == 0 {
                        retry = true;
                        continue;
                    }
                    wait_on_buffer(this);
                }
                // 解锁却不 uptodate 且不脏 = 发生过 I/O 错误。
                // 用裸指针读这几个标志：`bh()` 的 `&mut` 在下面 `ll_rw_block`
                // 里还会被再取一次（同一个缓冲），两条 `&mut` 重叠时
                // `b_count += 1` / `-= 1` 这对读—改—写有可能各自读到过期值，
                // 结果是计数不平衡（观察到 `b_count` 减到下溢 panic）。
                {
                    let bp = buf_ptr(this);
                    if wait
                        && (*bp).b_req
                        && !(*bp).b_lock
                        && !(*bp).b_dirt
                        && !(*bp).b_uptodate
                    {
                        err = true;
                        continue;
                    }
                    // 第三趟只等，不写
                    if !(*bp).b_dirt || pass >= 2 {
                        continue;
                    }
                }
                // 原版在 ll_rw_block 前后 b_count++/-- 保护缓冲不被复用
                (*buf_ptr(this)).b_count += 1;
                ll_rw_block(WRITE, &mut [this]);
                (*buf_ptr(this)).b_count -= 1;
                retry = true;
            }
            if !(wait && retry && pass < 2) {
                break;
            }
            pass += 1;
        }
    }
    err
}

/// 对应原版 `sync_dev()`。`sync_supers`/`sync_inodes` 在各自模块里。
///
/// # Safety
/// 不能在中断上下文调用。
pub unsafe fn sync_dev(dev: u16) {
    // SAFETY: 契约转交。
    unsafe {
        sync_buffers(dev, false);
        crate::fs::super_block::sync_supers(dev);
        crate::fs::inode::sync_inodes(dev);
        sync_buffers(dev, false);
    }
}

/// 对应原版 `fsync_dev()`：比 `sync_dev` 多的是最后一趟带 `wait`。
///
/// # Safety
/// 不能在中断上下文调用。
pub unsafe fn fsync_dev(dev: u16) -> bool {
    // SAFETY: 契约转交。
    unsafe {
        sync_buffers(dev, false);
        crate::fs::super_block::sync_supers(dev);
        crate::fs::inode::sync_inodes(dev);
        sync_buffers(dev, true)
    }
}

/// 丢弃某设备的所有缓存。对应原版 `invalidate_buffers()`。
/// 用在卸载与介质更换之后。
///
/// # Safety
/// 不能在中断上下文调用。调用前应先 `sync_dev`，否则脏数据丢失。
pub unsafe fn invalidate_buffers(dev: u16) {
    // SAFETY: 契约转交。
    unsafe {
        let mut p = *core::ptr::addr_of!(FREE_LIST);
        for _ in 0..*core::ptr::addr_of!(NR_BUFFERS_USED) {
            if p == NIL {
                break;
            }
            let this = p;
            p = bh(this).b_next_free;
            if (*buf_ptr(this)).b_dev != dev {
                continue;
            }
            wait_on_buffer(this);
            let b = bh(this);
            // 原版这里再确认一次 b_dev（睡眠期间可能已被复用）
            if b.b_dev == dev {
                b.b_uptodate = false;
                b.b_dirt = false;
                b.b_req = false;
            }
        }
    }
}

/// 块大小设置。对应原版 `set_blocksize()`，但只接受 [`BLOCK_SIZE`]。
///
/// 原版支持 512/1024/2048/4096 并在切换时回写+丢弃旧尺寸的缓冲。
/// 我们的两个块设备（ramdisk、将来的 hd）和 minix 都固定用 1024，
/// 支持多尺寸会引入「同一设备同时存在不同 b_size 的缓冲」这条路径
/// 而没有任何调用方去走，所以只留下参数校验。
pub fn set_blocksize(dev: u16, size: usize) {
    if size != BLOCK_SIZE {
        pr_warn!("VFS: set_blocksize({}/{}, {}): only {} supported",
                 major(dev), minor(dev), size, BLOCK_SIZE);
    }
}

/// 缓存统计。原版 `show_buffers()` 打得更细（分 used/locked/dirty）。
pub fn show_buffers() {
    let mut used = 0;
    let mut locked = 0;
    let mut dirty = 0;
    // SAFETY: 只读缓冲头的标量字段。
    unsafe {
        for i in 0..*core::ptr::addr_of!(NR_BUFFERS_USED) {
            let b = bh(i);
            if b.b_count != 0 {
                used += 1;
            }
            if b.b_lock {
                locked += 1;
            }
            if b.b_dirt {
                dirty += 1;
            }
        }
        pr_info!("Buffer memory: {}K, {} buffers, {} used, {} locked, {} dirty",
                 *core::ptr::addr_of!(NR_BUFFERS_USED) * BLOCK_SIZE / 1024,
                 *core::ptr::addr_of!(NR_BUFFERS_USED), used, locked, dirty);
    }
}

/// 已建立的缓冲数。
pub fn nr_buffers() -> usize {
    // SAFETY: 只读一个 usize。
    unsafe { *core::ptr::addr_of!(NR_BUFFERS_USED) }
}

/// 建立缓冲空闲环。对应原版 `buffer_init()` + `grow_buffers()`。
///
/// 原版 `grow_buffers` 每次取一页、切成 `PAGE_SIZE/size` 个缓冲、
/// 用 `b_this_page` 串成环便于整页回收。我们一次分配完
/// [`NR_BUFFERS`]/4 页（1024 字节块，每页 4 个），不回收，所以不需要
/// `b_this_page`。
///
/// # Safety
/// 启动期调用一次，此时页分配器已就绪且没有其他任务在跑 fs 代码。
pub unsafe fn init() {
    let per_page = PAGE_SIZE / BLOCK_SIZE;
    // SAFETY: 契约保证独占；页分配器已 init。
    unsafe {
        let mut built = 0;
        while built < NR_BUFFERS {
            let page = page_alloc::get_free_page();
            if page == 0 {
                // 原版 buffer_init: `panic("VFS: Unable to initialize buffer free list!")`
                if built == 0 {
                    panic!("VFS: Unable to initialize buffer free list!");
                }
                break;
            }
            for k in 0..per_page {
                if built >= NR_BUFFERS {
                    break;
                }
                let n = built;
                // 裸指针：下面的重复页检查要读整张 BUFFERS，而 `bh(n)` 会
                // 返回一个活着的 `&mut`，两条路径同时存在就是 noalias UB。
                // 踩过一次：那次的表现是 `assert!(page >= 0x10_0000)` 用一个
                // 明明大于 1MB 的值触发了失败——编译器基于 noalias 假设重排
                // 了比较与格式化里的两次读。
                // 缓冲数据页必须在 1MB 之上：低端内存塞满了启动期结构和
                // BIOS 洞（0xF0000 是 BIOS ROM）。拿到低端页说明页分配器
                // 的保留逻辑破了；不挡的话症状是缓冲里读出 BIOS ROM 的
                // 内容（`0xf000ff53` 那串 IRET stub），完全看不出源头。
                assert!(
                    page >= 0x10_0000,
                    "buffer: got low-memory page {:#x} from allocator",
                    page
                );
                // 同一页被派两次的话，两个缓冲头的 b_data 会重叠：写一个块
                // 就改到了另一个块，症状是随机丢失的写和「readdir 里 `.`
                // 不见了」这类内容损坏，而「一块一缓冲」的哈希链检查看不
                // 出来（dev/blocknr 各不相同，只有数据页重叠）。
                // 只比对**之前那些页**的缓冲：本页内的 per_page 个缓冲
                // 当然共用这一页（k 就是页内序号），那不是重复。
                // `built - k` 不对：`k` 是本页内序号，而 `built` 在页与页
                // 之间连续累加，本页的第一个缓冲下标是 `built - k` 只在
                // 本页从 0 开始填时成立。直接算本页起点。
                let page_first = built - k;
                for j in 0..page_first {
                    // volatile + 裸指针：`(*addr_of!(BUFFERS))[j]` 建的是共享
                    // 引用，而这一轮循环后面就要通过 `buf_ptr(n)` 写同一张表，
                    // 重叠的共享/可变引用会让这次读不可信。
                    let od = core::ptr::read_volatile(
                        core::ptr::addr_of!((*buf_ptr(j)).b_data)
                    ) as usize;
                    assert!(
                        od & !(PAGE_SIZE - 1) != page,
                        "buffer: page {:#x} handed out twice (buf {} already has {:#x})",
                        page, j, od
                    );
                }
                let pb = buf_ptr(n);
                (*pb).b_data = (page + k * BLOCK_SIZE) as *mut u8;
                (*pb).b_size = BLOCK_SIZE;
                (*pb).b_dev = 0;
                (*pb).b_count = 0;
                // 先自成环，再挂进全局环。用裸指针写（这里 n 就是自己的
                // 前驱和后继，`&mut` 会重叠）——见 [`buf_ptr`]。
                if *core::ptr::addr_of!(FREE_LIST) == NIL {
                    let pn = buf_ptr(n);
                    (*pn).b_next_free = n;
                    (*pn).b_prev_free = n;
                    *core::ptr::addr_of_mut!(FREE_LIST) = n;
                } else {
                    insert_at_free_tail(n);
                }
                built += 1;
            }
        }
        *core::ptr::addr_of_mut!(NR_BUFFERS_USED) = built;
        // 建少了要说出来。`NR_BUFFERS_USED < NR_BUFFERS` 时，表里剩下的
        // 缓冲头 b_data 仍是空指针，而 getblk 只按 b_count/b_size 挑候选、
        // 不会检查 b_data——一旦这种缓冲被选中，data() 就会拿空指针去
        // 构造切片（症状是 slice/iter.rs 里的 assert_unchecked 失败或
        // from_raw_parts 报空指针）。
        if built < NR_BUFFERS {
            pr_warn!("VFS: only {} of {} buffers built (low memory)", built, NR_BUFFERS);
        }
        // 建完之后逐个复核：`b_data` 为空或 `b_size` 不对的缓冲一旦被
        // `getblk` 选中，`data()` 就会拿空指针构造切片。这里查一遍比等到
        // 那时候再报便宜得多——那时已经看不出是 init 漏了谁。
        for i in 0..built {
            let p = buf_ptr(i);
            assert!(
                !(*p).b_data.is_null() && (*p).b_size == BLOCK_SIZE,
                "buffer init: buf {} of {} has b_data={:?} b_size={}",
                i, built, (*p).b_data, (*p).b_size
            );
        }
        check_free_ring("init:exit");
        pr_info!("Buffer cache: {} buffers of {} bytes ({}K)",
                 built, BLOCK_SIZE, built * BLOCK_SIZE / 1024);
    }
}

/// 唤醒等空闲缓冲的任务。给驱动层的 `end_request` 用。
///
/// # Safety
/// 可在中断上下文调用（只改任务状态）。
pub unsafe fn wake_buffer_waiters() {
    // SAFETY: wake_up 不睡。
    unsafe { (*core::ptr::addr_of_mut!(BUFFER_WAIT)).wake_up() }
}

/// 让某个任务因 I/O 完成而变 Running。给 `end_request` 用（原版
/// `end_request` 里 `p->state = TASK_RUNNING` 那两行）。
///
/// # Safety
/// `nr` 必须是有效任务下标。可在中断上下文调用。
pub unsafe fn wake_io_waiter(nr: usize) {
    // SAFETY: 契约保证下标有效；只改状态位。
    unsafe {
        let t = task(nr);
        t.state = TaskState::Running;
        if t.counter > task(current_nr()).counter {
            crate::sched::set_need_resched();
        }
    }
}

/// 关中断执行一段临界区。原版直接写 `cli()`/`sti()`。
///
/// # Safety
/// `f` 里不能睡。
#[inline]
pub unsafe fn with_irq_off<T>(f: impl FnOnce() -> T) -> T {
    // SAFETY: 契约转交；save/restore 配对。
    unsafe {
        let flags = irq::local_irq_save();
        let r = f();
        irq::restore_flags(flags);
        r
    }
}

/// `schedule()` 的转发，给驱动层等 I/O 用。
///
/// # Safety
/// 不能在中断上下文调用。
pub unsafe fn io_schedule() {
    // SAFETY: 契约转交。
    unsafe { schedule() }
}
