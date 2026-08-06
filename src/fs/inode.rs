//! inode 表。对应 linux-1.0.9 的 `fs/inode.c` 与 `fs.h` 的 `struct inode`。
//!
//! # 与原版的结构性差异
//!
//! 1. **定长表 + 下标链**。原版 inode 也是动态生长的
//!    （`grow_inodes()` 每次拿一页切成 `PAGE_SIZE/sizeof(struct inode)` 个），
//!    带 `first_inode` 环、`i_hash_next/prev` 哈希链、`nr_free_inodes` 计数。
//!    我们固定 [`NR_INODE`] 项，直接线性扫描找空闲项 —— 64 项的扫描
//!    比维护两条链更简单，且没有 `grow_inodes` 就没有「扫描时表在变长」
//!    这条竞态路径。哈希链因此也省掉了（原版 `NR_IHASH=131` 是为了在
//!    2048 个 inode 里快速查找）。
//! 2. **`i_op` 函数指针表 → [`FsType`] 枚举**（见 `fs/mod.rs` 文档第 2 点）。
//! 3. **`union u` → `data: [u16; 9]`**：正好装 minix 的 `i_zone[9]`
//!    （原版 `minix_inode_info` 就是这个数组）。
//! 4. **`i_sem` 换成 `i_lock: bool` + `i_wait`**：原版 1.0.9 的 `i_sem`
//!    其实也只当互斥锁用（`down`/`up` 一对一），而 `lock_inode`/
//!    `unlock_inode` 走的是 `i_lock` + `i_wait`。我们只保留后者。
//! 5. 没有 `i_pipe`/`i_mmap`/`i_socket`/`i_flock`：对应的
//!    `fs/pipe.c`、`mm/mmap.c`、网络、`fs/locks.c` 都没移植。
//!    `iput` 里那两段（唤醒管道读者、释放管道页）随之删掉。

use crate::fs::buffer;
use crate::fs::{NR_INODE, major, minor, mode};
use crate::klib::errno::EINVAL;
use crate::sched::WaitQueue;
use crate::{pr_info, pr_warn};

/// 空下标。同 [`buffer::NIL`]。
pub const NIL: usize = usize::MAX;

/// 文件系统类型。原版对应 `inode->i_op` 指向哪张 `inode_operations`。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsType {
    /// 还没绑定（原版 `i_op == NULL`）
    None,
    /// minix 文件系统。原版 `minix_inode_operations`
    Minix,
    /// ext2/ext3/ext4 文件系统。原版 `ext2_inode_operations`
    Ext2,
    /// 字符设备文件。原版 `chrdev_inode_operations`
    Chr,
    /// 块设备文件。原版 `blkdev_inode_operations`
    Blk,
}

/// 内存里的 inode。对应原版 `struct inode`。
pub struct Inode {
    /// 所在设备。原版 `dev_t i_dev`
    pub i_dev: u16,
    /// inode 号。原版 `unsigned long i_ino`
    pub i_ino: u32,
    /// 类型与权限。原版 `umode_t i_mode`
    pub i_mode: u16,
    /// 硬链接数。原版 `nlink_t i_nlink`
    pub i_nlink: u16,
    /// 属主。原版 `uid_t i_uid`
    pub i_uid: u16,
    /// 属组。原版 `gid_t i_gid`
    pub i_gid: u16,
    /// 设备号（仅设备文件有意义）。原版 `dev_t i_rdev`
    pub i_rdev: u16,
    /// 文件大小。原版 `off_t i_size`
    pub i_size: u32,
    /// 访问时间。原版 `time_t i_atime`
    pub i_atime: u32,
    /// 修改时间。原版 `i_mtime`
    pub i_mtime: u32,
    /// inode 变更时间。原版 `i_ctime`
    pub i_ctime: u32,
    /// 块大小。原版 `unsigned long i_blksize`
    pub i_blksize: u32,

    /// 引用计数。原版 `unsigned short i_count`
    pub i_count: u16,
    /// 内容比磁盘新。原版 `unsigned char i_dirt`
    pub i_dirt: bool,
    /// 正在读/写这个 inode。原版 `unsigned char i_lock`
    pub i_lock: bool,
    /// 等 `i_lock` 的任务。原版 `struct wait_queue * i_wait`
    pub i_wait: WaitQueue,
    /// 挂载标志（从 super_block 继承）。原版 `unsigned short i_flags`
    pub i_flags: u64,

    /// 所属超级块的下标。原版 `struct super_block * i_sb`
    pub i_sb: usize,
    /// 这个 inode 是某个挂载点，指向被挂上来的根 inode。
    /// 原版 `struct inode * i_mount`
    pub i_mount: usize,

    /// 该走哪套 inode 操作。原版 `struct inode_operations * i_op`
    pub i_op: FsType,

    /// 文件系统私有数据。minix 用作 `i_zone[9]`（7 直接 + 1 一级间接 +
    /// 1 二级间接）。原版 `union u` 里的 `minix_inode_info`。
    pub data: [u16; 9],

    /// 槽位是否在用。原版靠 `i_count` 与 `first_inode` 链区分，
    /// 我们是定长表，需要一个显式标记（同 `TaskState::Unused` 的用意）。
    pub in_use: bool,
}

impl Inode {
    const fn new() -> Self {
        Inode {
            i_dev: 0,
            i_ino: 0,
            i_mode: 0,
            i_nlink: 0,
            i_uid: 0,
            i_gid: 0,
            i_rdev: 0,
            i_size: 0,
            i_atime: 0,
            i_mtime: 0,
            i_ctime: 0,
            i_blksize: 0,
            i_count: 0,
            i_dirt: false,
            i_lock: false,
            i_wait: WaitQueue::new(),
            i_flags: 0,
            i_sb: NIL,
            i_mount: NIL,
            i_op: FsType::None,
            data: [0; 9],
            in_use: false,
        }
    }

    /// 只读挂载？对应原版宏 `IS_RDONLY(inode)`。
    pub fn is_rdonly(&self) -> bool {
        if self.i_sb == NIL {
            return false;
        }
        // SAFETY: i_sb 非 NIL 说明它是 super_block 表里的有效下标。
        unsafe { super::super_block::sb(self.i_sb).s_flags & crate::fs::MS_RDONLY != 0 }
    }

    /// 是设备文件（字符或块）？
    pub fn is_device(&self) -> bool {
        mode::is_chr(self.i_mode) || mode::is_blk(self.i_mode)
    }
}

/// inode 表两侧的护栏，用来区分 bug-029 的两种可能：整块 memcpy 打偏
/// （护栏一起被冲掉）还是通过坏下标/坏指针的单字段写（护栏完好）。
/// 与 `super_block::SUPER_AREA` 的护栏同一套手法。
const INODE_GUARD: u64 = 0x5A5A_1234_ABCD_5A5A;

/// inode 表 + 两侧护栏。原版是动态链表（见模块文档第 1 点）。
///
/// 包在一个结构里而不是三个独立 static，是为了让护栏和表在内存里真的相邻
/// ——独立的 static 之间链接器可以任意插空、重排，护栏就守不住任何东西。
#[repr(C)]
struct InodeArea {
    guard_lo: [u64; 4],
    table: [Inode; NR_INODE],
    guard_hi: [u64; 4],
}

static mut INODE_AREA: InodeArea = InodeArea {
    guard_lo: [INODE_GUARD; 4],
    table: [const { Inode::new() }; NR_INODE],
    guard_hi: [INODE_GUARD; 4],
};

/// 检查两侧护栏。破了就报出哪一侧、第几个字。
///
/// # Safety
/// 只读；可在中断上下文调用。
pub unsafe fn watchdog_guards_ok() -> Option<(&'static str, usize, u64)> {
    // SAFETY: 只做 volatile 读，不建 `&mut`（中断里不能与进程上下文的
    // `&mut` 共存）。
    unsafe {
        let area = core::ptr::addr_of!(INODE_AREA);
        for k in 0..4 {
            let v = core::ptr::addr_of!((*area).guard_lo[k]).read_volatile();
            if v != INODE_GUARD {
                return Some(("lo", k, v));
            }
            let v = core::ptr::addr_of!((*area).guard_hi[k]).read_volatile();
            if v != INODE_GUARD {
                return Some(("hi", k, v));
            }
        }
        None
    }
}

/// 等空闲 inode 的任务。对应原版 `static struct wait_queue * inode_wait`。
static mut INODE_WAIT: WaitQueue = WaitQueue::new();

/// 临时看门狗：扫 inode 表找被 IVT/BIOS 字节灌过的槽。
///
/// 与 [`super_block::watchdog_bad_dev`] 同一套手法，用来定位 bug-029：
/// 失败日志里出现过 `i_mode=0o177523`（0xFF53）、`i_nlink=255`、
/// `st_size=0xF000F84D`——都是低端内存那串 `0xf000ff53` IRET stub 的碎片，
/// 说明有人把 IVT/BIOS ROM 的内容写进了 inode 表。由 `do_timer` 每滴答调一次，
/// 抓到就把被打断的 RIP 打出来。只在 debug 构建里跑。
///
/// 返回 `(槽号, 坏掉的 i_mode)`。
///
/// # Safety
/// 只读 inode 表；可以在中断上下文调用。
pub unsafe fn watchdog_bad_inode() -> Option<(usize, u16)> {
    // SAFETY: 只做 volatile 读，不建 `&mut`（中断里不能与进程上下文的
    // `&mut` 共存，见 buffer::init 里关于 noalias 的注释）。
    unsafe {
        let base = core::ptr::addr_of!((*core::ptr::addr_of!(INODE_AREA)).table).cast::<Inode>();
        for n in 0..NR_INODE {
            let p = base.add(n);
            let count = core::ptr::addr_of!((*p).i_count).read_volatile();
            if count == 0 {
                // 空闲槽的内容不作数（i_mode 可能是上一个使用者的残留）。
                continue;
            }
            let m = core::ptr::addr_of!((*p).i_mode).read_volatile();
            // `i_mode == 0` 不算坏：`iget` 先占住槽（i_count=1）再由
            // `read_inode` 去读盘填字段，中间有一个 i_mode 还是 0 的窗口，
            // 而这个看门狗是从时钟中断里扫的，正好会撞上。只查确实是垃圾
            // 的形态：高 4 位全 1（0xF000/0xFF53 那串 IVT 字节的特征）。
            if m != 0 && m & 0xF000 == 0xF000 {
                return Some((n, m));
            }
            // nlink 同理：只有 i_mode 已填好（说明 read_inode 跑完了）才查。
            if m != 0 {
                let nlink = core::ptr::addr_of!((*p).i_nlink).read_volatile();
                if nlink == 0xFF || nlink == 0xFFFF {
                    return Some((n, m));
                }
            }
        }
        None
    }
}

/// 取 inode。
///
/// # Safety
/// `n < NR_INODE`；调用者需保证不与其他 `&mut` 别名同时存在。
#[inline]
#[track_caller]
pub unsafe fn inode(n: usize) -> &'static mut Inode {
    // 越界立刻报出下标。这里最常见的错因是把 NIL（usize::MAX）当下标传进来
    // 或者用了一个已经 iput 掉的索引；不挡的话表现为随机位置的 page fault，
    // 定位成本高得多。
    assert!(n < NR_INODE, "inode(): index {} out of range (NR_INODE={})", n, NR_INODE);
    // SAFETY: 上面已校验下标在界内；单核内核。
    unsafe { &mut (*core::ptr::addr_of_mut!(INODE_AREA)).table[n] }
}

/// inode 的裸指针。
///
/// 一条表达式里要碰同一个 inode 两次（`(*inode_ptr(n)).i_count += 1` 之外的
/// 任何组合，比如 `inode(n).i_sb != NIL && inode(n).i_op == ...` 或者
/// `inode::(*inode_ptr(a)).i_dev != inode::(*inode_ptr(b)).i_dev` 且 `a == b` 的情形）
/// 就必须用这个。[`inode`] 返回 `&'static mut Inode`，两条 `&mut` 指向
/// 同一对象时 `noalias` 允许 LLVM 假定彼此不可见——写会被合并或丢弃，
/// 读会拿到过期值。表现为 `i_count` 溢出、`i_mount` 变成垃圾下标、
/// inode 的 `data[]`（zone 表）里出现别处的字节
/// （症状是 `minix_getblk: block>big` 洪水）。
///
/// 同一类错误在 [`crate::fs::buffer::buf_ptr`]、
/// [`crate::fs::super_block`] 和 [`crate::sched`] 里都修过。
///
/// # Safety
/// `n < NR_INODE`（内部断言）；调用者负责独占（进程上下文 + i_lock）。
#[inline]
#[track_caller]
pub unsafe fn inode_ptr(n: usize) -> *mut Inode {
    assert!(n < NR_INODE, "inode_ptr(): index {} out of range", n);
    // SAFETY: 下标已校验；INODES 地址恒定。
    unsafe { (*core::ptr::addr_of_mut!(INODE_AREA)).table.as_mut_ptr().add(n) }
}

/// 等 `i_lock` 放开。对应原版 `wait_on_inode()`。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。
#[track_caller]
pub unsafe fn wait_on_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        // 用 sleep_on_while：先挂队列再判条件，否则 unlock_inode 的
        // wake_up 可能落在「判完 i_lock」和「挂上队列」之间而丢失，
        // 任务永久睡死。见 sched::WaitQueue::sleep_on_while。
        // 两个裸指针分别指向等待队列和锁标志：不能同时通过 inode() 取
        // `&mut i_wait` 和读 `i_lock`，那是对同一个 static mut 的重叠可变
        // 借用，优化后会读到过期的 i_lock（踩过一次，症状是直接三重错误）。
        assert!(n < NR_INODE, "wait_on_inode(): index {} out of range", n);
        let ip = inode_ptr(n);
        let wq = core::ptr::addr_of_mut!((*ip).i_wait);
        let lock = core::ptr::addr_of!((*ip).i_lock);
        (*wq).sleep_on_while(|| core::ptr::read_volatile(lock));
    }
}

/// 对应原版 `lock_inode()`。
///
/// # Safety
/// 同 [`wait_on_inode`]。
pub unsafe fn lock_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        wait_on_inode(n);
        inode(n).i_lock = true;
    }
}

/// 对应原版 `unlock_inode()`。
///
/// # Safety
/// `n < NR_INODE`。
pub unsafe fn unlock_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        inode(n).i_lock = false;
        inode(n).i_wait.wake_up();
    }
}

/// 清空一个 inode 槽位。对应原版 `clear_inode()`。
///
/// 原版会先把 `i_wait`/`i_next`/`i_prev` 存下来、`memset` 整个结构、
/// 再把链指针写回去 —— 因为链是结构体的一部分。我们的 `WaitQueue`
/// 也必须保留（可能有任务正睡在上面），所以同样是「保留等待队列、
/// 清其余字段」。
///
/// # Safety
/// `n < NR_INODE`，且该 inode 的 `i_count == 0`。
pub unsafe fn clear_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let i = inode(n);
        // 原版把 i_wait 抢救出来再 memset：睡在上面的任务不能丢
        let saved_wait = core::mem::replace(&mut i.i_wait, WaitQueue::new());
        *i = Inode::new();
        i.i_wait = saved_wait;
    }
}

/// 找一个空闲 inode 槽位。对应原版 `get_empty_inode()`。
///
/// 原版的挑选逻辑：优先取 `i_count == 0 && !i_dirt && !i_lock` 的，
/// 退而取任何 `i_count == 0` 的（然后回写/等锁再重试），一个都没有就
/// `grow_inodes()` 扩容，扩不动才 `sleep_on(&inode_wait)`。
/// 我们没有扩容，所以是「挑最好的 → 处理脏/锁 → 都不行就睡等」。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。
pub unsafe fn get_empty_inode() -> usize {
    // SAFETY: 契约转交。
    unsafe {
        loop {
            let mut best = NIL;
            for n in 0..NR_INODE {
                let i = inode(n);
                if i.i_count != 0 {
                    continue;
                }
                if best == NIL {
                    best = n;
                }
                if !i.i_dirt && !i.i_lock {
                    best = n;
                    break;
                }
            }
            if best == NIL {
                // 原版：printk("VFS: No free inodes - contact Linus") 然后睡
                pr_warn!("VFS: No free inodes");
                (*core::ptr::addr_of_mut!(INODE_WAIT)).sleep_on();
                continue;
            }
            if inode(best).i_lock {
                wait_on_inode(best);
                continue;
            }
            if inode(best).i_dirt {
                write_inode(best);
                continue;
            }
            // 睡过之后可能被别人抢走了
            if (*inode_ptr(best)).i_count != 0 {
                continue;
            }
            clear_inode(best);
            let i = inode(best);
            i.i_count = 1;
            i.i_nlink = 1;
            i.in_use = true;
            return best;
        }
    }
}

/// 从磁盘读入 inode 内容。对应原版 `read_inode()`
/// （转发到 `sb->s_op->read_inode`）。
///
/// # Safety
/// 只能在进程上下文调用。`n` 的 `i_sb`/`i_ino` 必须已填好。
unsafe fn read_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        lock_inode(n);
        let sb_nr = inode(n).i_sb;
        if sb_nr != NIL {
            // 原版是 s_op->read_inode 这个函数指针；我们只有 minix
            super::minix::read_inode(n);
        }
        unlock_inode(n);
    }
}

/// 回写一个 inode。对应原版 `write_inode()`
/// （转发到 `sb->s_op->write_inode`）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn write_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        if !inode(n).i_dirt {
            return;
        }
        lock_inode(n);
        // 原版：只读挂载的文件系统上 write_inode 是 no-op
        let ip = inode_ptr(n);
        if !(*ip).is_rdonly() && (*ip).i_sb != NIL {
            super::minix::write_inode(n);
        }
        inode(n).i_dirt = false;
        unlock_inode(n);
    }
}

/// 按 (超级块, inode 号) 取一个 inode，引用计数 +1。
/// 对应原版 `iget()` / `__iget()`。
///
/// `cross_mnt` 为真时，如果这个 inode 是挂载点，返回挂上来的那个文件系统的
/// 根 inode（原版 `__iget(sb, nr, crossmntp)` 的第三个参数，`iget` 传 1）。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。`sb_nr` 必须是有效超级块下标。
pub unsafe fn iget_cross(sb_nr: usize, ino: u32, cross_mnt: bool) -> usize {
    if sb_nr == NIL {
        panic!("VFS: iget with sb==NULL");
    }
    // SAFETY: 契约转交。
    unsafe {
        let dev = super::super_block::sb(sb_nr).s_dev;
        loop {
            // 先查表里有没有现成的（原版走哈希链，我们线性扫，见文档第 1 点）
            let mut found = NIL;
            for n in 0..NR_INODE {
                // 裸指针：`let i = inode(n)` 会留一条活着的 `&mut`，而循环
                // 后面紧接着又对同一张表取 `inode(found)`，两条重叠的
                // `&mut` 带 noalias，编译器可以丢掉其中一条上的写。实测
                // 症状是 `i_count += 1` 溢出（读到的是别处缓存的旧值）。
                let ip = inode_ptr(n);
                if (*ip).in_use && (*ip).i_dev == dev && (*ip).i_ino == ino {
                    found = n;
                    break;
                }
            }

            if found != NIL {
                (*inode_ptr(found)).i_count += 1;
                wait_on_inode(found);
                // 睡的时候这个槽位可能被换掉了（原版那句
                // "Whee.. inode changed from under us"）
                let fp = inode_ptr(found);
                if (*fp).i_dev != dev || (*fp).i_ino != ino {
                    pr_warn!("VFS: inode changed from under us");
                    iput(found);
                    continue;
                }
                let mnt = (*inode_ptr(found)).i_mount;
                if cross_mnt && mnt != NIL {
                    let m = mnt;
                    // `i_mount` 只该由 do_mount/do_umount 写成一个有效槽位
                    // 下标或 NIL。越界值说明这个 inode 槽位被复用时没清干净，
                    // 或者被越界写打过；直接 inode(m) 的话报出来的是
                    // 「inode(): index ... out of range」，看不出是 i_mount。
                    assert!(
                        m < NR_INODE,
                        "iget: inode {} has corrupt i_mount {:#x} (dev={:#06x} ino={})",
                        found, m, (*inode_ptr(found)).i_dev, (*inode_ptr(found)).i_ino
                    );
                    (*inode_ptr(m)).i_count += 1;
                    iput(found);
                    wait_on_inode(m);
                    return m;
                }
                return found;
            }

            // 没有，建一个新的
            let empty = get_empty_inode();
            {
                let sbf = super::super_block::sb(sb_nr).s_flags;
                let i = inode(empty);
                i.i_sb = sb_nr;
                i.i_dev = dev;
                i.i_ino = ino;
                i.i_flags = sbf;
                i.i_blksize = buffer::BLOCK_SIZE as u32;
            }
            read_inode(empty);
            return empty;
        }
    }
}

/// 对应原版 `iget()`（即 `__iget(sb, nr, 1)`）。
///
/// # Safety
/// 同 [`iget_cross`]。
#[inline]
pub unsafe fn iget(sb_nr: usize, ino: u32) -> usize {
    // SAFETY: 契约转交。
    unsafe { iget_cross(sb_nr, ino, true) }
}

/// 归还一个 inode 引用。对应原版 `iput()`。
///
/// 原版的关键顺序（照搬）：计数 >1 就只减一；否则要先让文件系统处理
/// （`put_inode` —— minix 在那里对 `i_nlink == 0` 的 inode 真正释放
/// 数据块并回收 inode 位图），再看脏不脏决定要不要回写，回写会睡，
/// 睡醒要重新走一遍（那个 `goto repeat`）。
///
/// # Safety
/// 只能在进程上下文调用。`n` 必须是 [`iget`] 返回过的下标。
pub unsafe fn iput(n: usize) {
    if n == NIL {
        return;
    }
    // SAFETY: 契约转交。
    unsafe {
        wait_on_inode(n);
        if (*inode_ptr(n)).i_count == 0 {
            let i = inode(n);
            pr_warn!("VFS: iput: trying to free free inode");
            pr_warn!(
                "VFS: device {}/{}, inode {}, mode=0{:07o}",
                major(i.i_rdev),
                minor(i.i_rdev),
                i.i_ino,
                i.i_mode
            );
            return;
        }
        loop {
            if (*inode_ptr(n)).i_count > 1 {
                (*inode_ptr(n)).i_count -= 1;
                return;
            }
            (*core::ptr::addr_of_mut!(INODE_WAIT)).wake_up();

            // 原版 s_op->put_inode：minix 在 i_nlink == 0 时 truncate + free_inode
            let ip = inode_ptr(n);
            if (*ip).i_sb != NIL && (*ip).i_op == FsType::Minix {
                super::minix::put_inode(n);
                if (*inode_ptr(n)).i_nlink == 0 {
                    // put_inode 已经把它释放并 clear_inode 了
                    return;
                }
            }
            if inode(n).i_dirt {
                write_inode(n);
                wait_on_inode(n);
                continue; // 原版 goto repeat
            }
            (*inode_ptr(n)).i_count -= 1;
            if (*inode_ptr(n)).i_count == 0 {
                inode(n).in_use = false;
            }
            return;
        }
    }
}

/// 回写某设备上所有脏 inode。对应原版 `sync_inodes()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sync_inodes(dev: u16) {
    // SAFETY: 契约转交。
    unsafe {
        for n in 0..NR_INODE {
            if !inode(n).in_use {
                continue;
            }
            if dev != 0 && (*inode_ptr(n)).i_dev != dev {
                continue;
            }
            wait_on_inode(n);
            if inode(n).i_dirt {
                write_inode(n);
            }
        }
    }
}

/// 丢弃某设备上的所有 inode。对应原版 `invalidate_inodes()`，
/// 卸载时用。返回是否有 inode 还在被引用（原版 `busy`）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn invalidate_inodes(dev: u16) -> bool {
    let mut busy = false;
    // SAFETY: 契约转交。
    unsafe {
        for n in 0..NR_INODE {
            let ip = inode_ptr(n);
            if !(*ip).in_use || (*ip).i_dev != dev {
                continue;
            }
            wait_on_inode(n);
            if (*inode_ptr(n)).i_count != 0 {
                busy = true;
            } else {
                clear_inode(n);
            }
        }
    }
    busy
}

/// 权限检查。对应原版 `fs/namei.c` 的 `permission()`
/// （原版会先试 `i_op->permission`，minix 没有实现所以走通用逻辑）。
///
/// 原版通用逻辑：root（`suser()`）除「对没有任何 x 位的文件要求执行」
/// 之外无条件通过；否则按 uid/gid 选 owner/group/other 三档权限位比对。
///
/// 我们目前没有用户态、`current->uid` 恒为 0（root），所以这个函数
/// 现在总是走 root 分支。保留完整逻辑是因为 `sys_open`/`namei` 里的
/// 调用点必须存在，等 `execve` 与 uid 就位后它立刻就是对的。
pub fn permission(n: usize, mask: u16) -> bool {
    // SAFETY: 调用点保证 n 有效；只读几个字段。
    let (i_mode, i_uid, i_gid) = unsafe {
        let i = inode(n);
        (i.i_mode, i.i_uid, i.i_gid)
    };
    // 原版 current->euid / egid；Task 里还没有这两个字段（见 STATUS.md），
    // 内核态一律当 root。
    let (euid, egid) = (0u16, 0u16);

    let mut m = i_mode;
    if euid == i_uid {
        m >>= 6;
    } else if egid == i_gid {
        m >>= 3;
    }
    if m & mask & 0o007 == mask {
        return true;
    }
    // root：读写无条件通过；执行要求至少有一个 x 位（同原版 suser() 分支）
    if euid == 0 {
        if mask & mode::S_IXUSR == 0 {
            return true;
        }
        if i_mode & 0o111 != 0 {
            return true;
        }
    }
    false
}

/// 建立 inode 表。对应原版 `inode_init()`。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn init() {
    // SAFETY: 契约保证独占。
    unsafe {
        for n in 0..NR_INODE {
            clear_inode(n);
        }
    }
    pr_info!("inode: {} slots", NR_INODE);
}

/// 在用的 inode 数与被引用的 inode 数。自检用（原版没有）。
pub fn stats() -> (usize, usize) {
    // SAFETY: 只读表；进程上下文调用。
    unsafe {
        let mut used = 0;
        let mut refd = 0;
        for n in 0..NR_INODE {
            let i = inode(n);
            if i.in_use {
                used += 1;
            }
            if i.i_count > 0 {
                refd += 1;
            }
        }
        (used, refd)
    }
}

/// 消掉未使用告警：`EINVAL` 在下一阶段的 `notify_change` 里会用到。
#[allow(dead_code)]
const _E: i32 = EINVAL;
