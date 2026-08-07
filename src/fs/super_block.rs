//! 超级块与挂载。对应 linux-1.0.9 的 `fs/super.c`。
//!
//! # 与原版的结构性差异
//!
//! 1. **只有 minix 一种文件系统**，所以原版的 `file_systems[]` 注册表
//!    （`fs/filesystems.c`）和 `get_fs_type()` 都不需要：`read_super`
//!    直接调 [`super::minix::read_super`]。原版那张表的存在是为了让
//!    `mount -t ext2` 能找到对应的 `read_super`，我们只有一个候选。
//! 2. **`union u` → 具名的 minix 字段**（见 `fs/mod.rs` 文档第 3 点）。
//!    minix 的位图缓冲（`s_imap[8]`/`s_zmap[8]`）在原版里是
//!    `struct buffer_head *`，这里是缓冲下标。
//! 3. **`s_covered`/`s_mounted` 保留**，这是挂载点的核心机制：
//!    `s_covered` 是被盖住的那个目录 inode，`s_mounted` 是新文件系统的
//!    根 inode，而被盖目录的 `i_mount` 指回 `s_mounted`。
//!    `iget` 的 `cross_mnt` 参数就是靠这个穿越挂载点。
//! 4. **不移植 `sys_umount` 的 `MS_REMOUNT`**（原版 `do_remount_sb`）
//!    与 `sys_ustat`：前者需要每个文件系统的 `remount_fs`，后者
//!    需要 `statfs` 那套。`sys_mount` 的正常路径实现了。

use crate::drivers::block::major::MEM_MAJOR;
use crate::fs::buffer::{self, NIL};
use crate::fs::inode::{self, FsType};
use crate::fs::{MS_RDONLY, NR_SUPER, major, minor, mode};
use crate::klib::errno::{EBUSY, EINVAL, ENODEV, ENOENT, ENOTBLK, ENOTDIR, EPERM};
use crate::sched::WaitQueue;
use crate::{pr_info, pr_notice, pr_warn};

/// minix 的位图槽位数。对应原版 `minix_fs.h` 的
/// `MINIX_I_MAP_SLOTS 8` / `MINIX_Z_MAP_SLOTS 8`。
pub const MINIX_I_MAP_SLOTS: usize = 8;
pub const MINIX_Z_MAP_SLOTS: usize = 8;

/// 超级块。对应原版 `struct super_block`，`union u` 展开成 minix 字段。
pub struct SuperBlock {
    /// 设备号，0 表示这个槽位空闲（同原版 `s_dev = 0` 的用法）。
    /// 原版 `dev_t s_dev`
    pub s_dev: u16,
    /// 块大小。原版 `unsigned long s_blocksize`
    pub s_blocksize: u32,
    /// 块大小的位数。原版 `unsigned char s_blocksize_bits`
    pub s_blocksize_bits: u8,
    /// 超级块本身被锁住。原版 `unsigned char s_lock`
    pub s_lock: bool,
    /// 只读。原版 `unsigned char s_rd_only`
    pub s_rd_only: bool,
    /// 超级块内容脏了要回写。原版 `unsigned char s_dirt`
    pub s_dirt: bool,
    /// 挂载标志（`MS_*`）。原版 `unsigned long s_flags`
    pub s_flags: u64,
    /// 文件系统魔数。原版 `unsigned long s_magic`
    pub s_magic: u32,
    /// 挂载时刻。原版 `unsigned long s_time`
    pub s_time: u32,
    /// 被这个文件系统盖住的目录 inode。原版 `struct inode * s_covered`
    pub s_covered: usize,
    /// 这个文件系统的根 inode。原版 `struct inode * s_mounted`
    pub s_mounted: usize,
    /// 等 `s_lock` 的任务。原版 `struct wait_queue * s_wait`
    pub s_wait: WaitQueue,

    // ---- 原版 `u.minix_sb`（`struct minix_sb_info`）----
    /// inode 总数。原版 `s_ninodes`
    pub s_ninodes: u16,
    /// 数据区总 zone 数。原版 `s_nzones`
    pub s_nzones: u16,
    /// inode 位图占几块。原版 `s_imap_blocks`
    pub s_imap_blocks: u16,
    /// zone 位图占几块。原版 `s_zmap_blocks`
    pub s_zmap_blocks: u16,
    /// 第一个数据 zone 的块号。原版 `s_firstdatazone`
    pub s_firstdatazone: u16,
    /// zone 大小 = block << s_log_zone_size。原版 `s_log_zone_size`
    pub s_log_zone_size: u16,
    /// 单文件最大字节数。原版 `s_max_size`
    pub s_max_size: u32,
    /// 目录项大小（16 或 32）。原版 `s_dirsize`
    pub s_dirsize: usize,
    /// 名字最大长度（14 或 30）。原版 `s_namelen`
    pub s_namelen: usize,
    /// 挂载前的 `s_state`，用来判断上次是否干净卸载。原版 `s_mount_state`
    pub s_mount_state: u16,
    /// 装着磁盘超级块的缓冲下标。原版 `s_sbh`
    pub s_sbh: usize,
    /// inode 位图的缓冲下标。原版 `struct buffer_head * s_imap[8]`
    pub s_imap: [usize; MINIX_I_MAP_SLOTS],
    /// zone 位图的缓冲下标。原版 `s_zmap[8]`
    pub s_zmap: [usize; MINIX_Z_MAP_SLOTS],
}

impl SuperBlock {
    const fn new() -> Self {
        SuperBlock {
            s_dev: 0,
            s_blocksize: 0,
            s_blocksize_bits: 0,
            s_lock: false,
            s_rd_only: false,
            s_dirt: false,
            s_flags: 0,
            s_magic: 0,
            s_time: 0,
            s_covered: NIL,
            s_mounted: NIL,
            s_wait: WaitQueue::new(),
            s_ninodes: 0,
            s_nzones: 0,
            s_imap_blocks: 0,
            s_zmap_blocks: 0,
            s_firstdatazone: 0,
            s_log_zone_size: 0,
            s_max_size: 0,
            s_dirsize: 0,
            s_namelen: 0,
            s_mount_state: 0,
            s_sbh: NIL,
            s_imap: [NIL; MINIX_I_MAP_SLOTS],
            s_zmap: [NIL; MINIX_Z_MAP_SLOTS],
        }
    }
}

/// 护栏图案。见 `SUPER_AREA`。
const SB_GUARD: u64 = 0x1234_ABCD_1234_ABCD;

/// 用 `#[repr(C)]` 保证内存布局：LO guard → 表 → HI guard。
/// 原先用三个独立 static，链接器把 GUARD_HI 放在了 SUPER_BLOCKS 前面、
/// GUARD_LO 放在了后面，完全失去保护作用（0x68bc0: HI, 0x68be0: LO,
/// 0x68c08: SUPER_BLOCKS）。越界写直接打到紧邻的 ROOT_INODE，根本碰不到
/// 任何护栏。
///
/// 起因：观察到整个 `SuperBlock` 被填成 BIOS ROM 的字节
/// （`s_dev=0xff53`、`s_imap[]` 里是 `0xf000ff53`），
/// 这种「一整片都坏」的形态只有内存被覆盖能解释。
#[repr(C)]
struct GuardedSuperBlocks {
    /// 低侧护栏，4 个 u64。
    lo: [u64; 4],
    /// 超级块表。
    table: [SuperBlock; NR_SUPER],
    /// 高侧护栏，4 个 u64。
    hi: [u64; 4],
}

/// 超级块表，含前后护栏。对应原版 `struct super_block super_blocks[NR_SUPER]`。
static mut SUPER_AREA: GuardedSuperBlocks = GuardedSuperBlocks {
    lo: [SB_GUARD; 4],
    table: [const { SuperBlock::new() }; NR_SUPER],
    hi: [SB_GUARD; 4],
};

/// 已挂载超级块的字段自检。`s_dirsize`/`s_namelen` 只能是 minix v1/v2 的
/// 那两组值；它们直接参与目录项偏移计算（`2 * dirsize`、`off + dirsize`），
/// 坏了表现为「乘法溢出」或「目录项写到别的块上」，离原因很远。
///
/// 由 [`mount_root`] 在挂载完成后调一次；不能放进 [`sb`]，因为
/// `minix::read_super` 填这两个字段的过程中它们必然短暂为 0。
///
/// # Safety
/// `n < NR_SUPER`；只读。
pub unsafe fn check_mounted(n: usize) {
    // 护栏在这里查：mount 边界上没有活着的 `&mut SuperBlock`（见 `sb()`
    // 里那条注释说明为什么不能放进访问器）。
    // SAFETY: 只读两个静态数组。
    unsafe { check_guards("check_mounted") };
    // SAFETY: 只读标量。
    unsafe {
        // 全部走裸指针 + volatile：`SUPER_AREA.table[n]` 建的是
        // 共享引用，一旦调用者手里还有同一槽的 `&mut`（mount_root 里就是
        // 这样），这次读就不可信。踩过两次：报出来的值是 IVT 字节
        // （0xf000ff53...）或超出槽数范围的下标，而看门狗每个滴答都在查
        // 同一块内存、从没发现异常——内存是好的，读坏了。
        let p = sb_ptr(n);
        let ds = core::ptr::read_volatile(core::ptr::addr_of!((*p).s_dirsize));
        let nl = core::ptr::read_volatile(core::ptr::addr_of!((*p).s_namelen));
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).s_magic));
        // ext4 没有 dirsize/namelen 概念，只校验 minix
        if magic != 0xEF53 {
            assert!(
                (ds == 16 && nl == 14) || (ds == 32 && nl == 30),
                "sb({}): dirsize/namelen corrupt: {}/{} (s_dev={:#06x} magic={:#x})",
                n, ds, nl,
                core::ptr::read_volatile(core::ptr::addr_of!((*p).s_dev)),
                magic
            );
        }
    }
}

/// 看门狗：`SUPER_AREA.table[0].s_dev` 的合法值只有 0（空闲）和 ROOT_DEV。
/// 由时钟中断每个滴答调一次，这样能把「被写坏」的时刻缩到一个滴答内，
/// 并且能在中断返回路径上拿到当时的 RIP（`do_timer` 收到 `PtRegs`）。
///
/// # Safety
/// 只读一个 u16，可在中断上下文调用。
pub unsafe fn watchdog_bad_dev() -> Option<u16> {
    // SAFETY: 只读。
    unsafe {
        let sp = &(*core::ptr::addr_of!(SUPER_AREA)).table[0];
        let d = core::ptr::read_volatile(core::ptr::addr_of!(sp.s_dev));
        if d != 0 && d != *core::ptr::addr_of!(ROOT_DEV) {
            return Some(d);
        }
        // 位图缓冲下标也一起看：观察到 s_dev 完好而 s_zmap[] 里出现
        // 低端内存的字节，说明写坏的范围不总是从结构开头开始。
        for &m in sp.s_imap.iter().chain(sp.s_zmap.iter()) {
            if m != crate::fs::buffer::NIL && m >= crate::fs::buffer::NR_BUFFERS {
                return Some(0xDEAD);
            }
        }
        // dirsize/namelen 也看：实测坏掉的正是这两个（整槽被灌成
        // 0xf000ff53... 的 IVT 字节）。只有 mount 完成后才有合法值，
        // 所以以 s_magic 是否已填为前提。
        let magic = core::ptr::read_volatile(core::ptr::addr_of!(sp.s_magic));
        if magic != 0 {
            let ds = core::ptr::read_volatile(core::ptr::addr_of!(sp.s_dirsize));
            let nl = core::ptr::read_volatile(core::ptr::addr_of!(sp.s_namelen));
            if !((ds == 16 && nl == 14) || (ds == 32 && nl == 30)) {
                return Some(0xDEAE);
            }
        }
        None
    }
}

/// 检查两侧护栏。破了就报出哪一侧、第几个字，并 panic。
///
/// # Safety
/// 只读，可在任何上下文调用。
pub unsafe fn check_guards(tag: &str) {
    // SAFETY: 只读。
    unsafe {
        // 全程裸指针，不建 `&SUPER_AREA`：那条共享引用覆盖整个结构（含
        // table），带 `dereferenceable`/`readonly`，和调用者可能持有的
        // `&mut SuperBlock` 重叠就是 UB。
        let base = core::ptr::addr_of_mut!(SUPER_AREA);
        let lo = core::ptr::addr_of_mut!((*base).lo) as *mut u64;
        let hi = core::ptr::addr_of_mut!((*base).hi) as *mut u64;
        for (name, p) in [("lo", lo), ("hi", hi)] {
            for i in 0..4 {
                let slot = p.add(i);
                let v = core::ptr::read_volatile(slot);
                if v != SB_GUARD {
                    // 复读一次：若复读是好的，说明内存完好、是这次比较不可信
                    // （或写入是瞬时的），和「真被写坏」要分开报。
                    let again = core::ptr::read_volatile(slot);
                    pr_warn!("super: guard {} word {} @ {:#x}: got {:#018x} want {:#018x} reread {:#018x} (at {})",
                             name, i, slot as usize, v, SB_GUARD, again, tag);
                    let dump = |q: *mut u64, n: &str| {
                        pr_warn!("super:   {} = [{:#018x} {:#018x} {:#018x} {:#018x}]",
                                 n,
                                 core::ptr::read_volatile(q),
                                 core::ptr::read_volatile(q.add(1)),
                                 core::ptr::read_volatile(q.add(2)),
                                 core::ptr::read_volatile(q.add(3)));
                    };
                    dump(lo, "lo");
                    dump(hi, "hi");
                    if again == SB_GUARD {
                        pr_warn!("super: reread OK -> memory intact, this compare was bogus; continuing");
                        continue;
                    }
                    panic!("SUPER_AREA guard {} smashed at {}", name, tag);
                }
            }
        }
    }
}

/// 根设备。对应原版 `dev_t ROOT_DEV`（由启动参数 `root=` 或
/// 编译期 `ROOT_DEV` 决定）。我们只有 ramdisk。
static mut ROOT_DEV: u16 = 0;

/// 根挂载标志。对应原版 `int root_mountflags`。
static mut ROOT_MOUNTFLAGS: u64 = 0;

/// 取超级块。
///
/// # Safety
/// `n < NR_SUPER`；调用者需保证不与其他 `&mut` 别名同时存在。
#[inline]
#[track_caller]
pub unsafe fn sb(n: usize) -> &'static mut SuperBlock {
    // 越界立刻报出下标。最常见的错因是把 NIL（usize::MAX）或一个未初始化的
    // `i_sb` 当下标传进来。没有这道检查的话越界写会落到 `SUPER_AREA`
    // 之后的静态变量上（BSS 里紧邻的就是别的表），症状是完全无关的地方
    // 读到垃圾——比如 `s_imap[]` 里出现低端内存 IVT 的 `0xf000ff53`。
    assert!(n < NR_SUPER, "sb(): index {} out of range (NR_SUPER={})", n, NR_SUPER);
    // 位图缓冲下标必须是有效缓冲下标或 NIL。这里查而不是等到 `bh(map)`
    // 才炸，是为了把「谁写坏了 s_imap/s_zmap」和「谁误用了它」分开：
    // 看到过 `0xf000ff53f000ff53`（低端内存 IVT 里那条 `F000:FF53` 的
    // 重复模式）出现在这两个数组里，说明有人把低端内存的内容拷进了
    // `SUPER_AREA`，而不是缓冲层算错了下标。
    // 护栏先查：如果是「越界写砸了整片」，护栏会先破，这样报出来的是
    // 「谁写坏了表」而不是「谁用了坏字段」。
    // 这里**不要**调 `check_guards()`。护栏检查本身要读静态数组，而
    // 调用者手里往往已经握着上一次 `sb(n)` 返回的 `&mut`；两条重叠的
    // 引用带 noalias，检查读到的值就不可信了。实测症状：报
    // "guard hi word 0 smashed: 0x1234abcd1234abcd"，而这个值恰好就是
    // 正确的 `SB_GUARD` —— 内存完好，是比较本身被编译器重排/复用了。
    // 护栏要查就在 mount/umount 这种不持有 `&mut` 的边界上查。
    // 这里曾经有一段「校验 s_imap/s_zmap 下标」的 debug 检查。**不要加
    // 回来**：它通过 `SUPER_AREA.table[n]` 建了一条共享引用，
    // 而调用者手里往往还握着上一次 `sb(n)` 返回的 `&mut`——共享引用与
    // `&mut` 重叠就是 UB，检查自己读到的值因此是垃圾。实测症状：报
    // "map slot 112 corrupt"，而 112 根本超出 8+8 个槽的范围，随后
    // dump 出来的内存又完好无损。字段级不变量要查就在具体的使用点
    // （见 `check_mounted`），不要放进访问器里。
    // SAFETY: 上面已校验下标在界内；单核内核。
    unsafe { &mut (*core::ptr::addr_of_mut!(SUPER_AREA)).table[n] }
}

/// 超级块的裸指针。
///
/// 连续改同一个超级块的多个字段时必须用这个，不要连着调 [`sb`]：
/// `*sb(n) = SuperBlock::new(); (*sb_ptr(n)).s_dev = dev;` 里两次 `sb(n)` 各产生
/// 一条 `&mut`，`&mut` 带 `noalias`，LLVM 可以假定第二条看不到第一条的
/// 写、反过来也一样，于是有机会把整体赋值排到字段赋值之后——`s_dev` 被
/// 清回 0，或者读到未初始化的字节。观察到的症状是 `s_dev=0xff53`
/// （低端内存 BIOS ROM 的字节）被拿去 `bread`，报
/// `ll_rw_block: Trying to read nonexistent block-device ff53`。
///
/// 同一类错误在 [`crate::fs::buffer::buf_ptr`] 和
/// [`crate::sched`] 的任务环里也修过。
///
/// # Safety
/// `n < NR_SUPER`；调用者负责独占（进程上下文 + s_lock）。
#[inline]
#[track_caller]
pub unsafe fn sb_ptr(n: usize) -> *mut SuperBlock {
    assert!(n < NR_SUPER, "sb_ptr(): index {} out of range", n);
    // SAFETY: 下标已校验；SUPER_AREA 地址恒定。
    unsafe { (*core::ptr::addr_of_mut!(SUPER_AREA)).table.as_mut_ptr().add(n) }
}

/// 根设备号。
pub fn root_dev() -> u16 {
    // SAFETY: 启动期设定后只读。
    unsafe { *core::ptr::addr_of!(ROOT_DEV) }
}

/// 等 `s_lock`。对应原版 `wait_on_super()`。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。
pub unsafe fn wait_on_super(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        // 同 inode::wait_on_inode：先挂队列再判条件，避免丢失唤醒。
        // 同 inode::wait_on_inode：先取裸指针，避免重叠可变借用。
        let sp = core::ptr::addr_of_mut!(*sb(n));
        let wq = core::ptr::addr_of_mut!((*sp).s_wait);
        let lock = core::ptr::addr_of!((*sp).s_lock);
        (*wq).sleep_on_while(|| core::ptr::read_volatile(lock));
    }
}

/// 对应原版 `lock_super()`。
///
/// # Safety
/// 同 [`wait_on_super`]。
pub unsafe fn lock_super(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        wait_on_super(n);
        sb(n).s_lock = true;
    }
}

/// 对应原版 `unlock_super()`。
///
/// # Safety
/// `n < NR_SUPER`。
pub unsafe fn unlock_super(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        sb(n).s_lock = false;
        sb(n).s_wait.wake_up();
    }
}

/// 按设备号找已挂载的超级块。对应原版 `get_super()`。
///
/// # Safety
/// 只能在进程上下文调用（`wait_on_super` 会睡）。
pub unsafe fn get_super(dev: u16) -> usize {
    if dev == 0 {
        return NIL;
    }
    // SAFETY: 契约转交。
    unsafe {
        loop {
            let mut found = NIL;
            for n in 0..NR_SUPER {
                if (*sb_ptr(n)).s_dev == dev {
                    found = n;
                    break;
                }
            }
            if found == NIL {
                return NIL;
            }
            wait_on_super(found);
            // 原版：睡的时候可能被 put_super 掉了，要重查
            if (*sb_ptr(found)).s_dev == dev {
                return found;
            }
        }
    }
}

/// 找一个空闲超级块槽位。对应原版 `read_super` 开头那个
/// `for (s = 0+super_blocks ;; s++) { if (s->s_dev == 0) break; }`。
fn get_empty_super() -> usize {
    // SAFETY: 只读 s_dev；进程上下文。
    unsafe {
        for n in 0..NR_SUPER {
            if (*sb_ptr(n)).s_dev == 0 {
                return n;
            }
        }
        NIL
    }
}

/// 读入一个文件系统的超级块。对应原版 `read_super()`。
///
/// 原版遍历 `file_systems[]` 找 `read_super`；我们只有 minix
/// （见模块文档第 1 点）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn read_super(dev: u16, flags: u64, silent: bool) -> usize {
    if dev == 0 {
        return NIL;
    }
    // SAFETY: 契约转交。
    unsafe {
        // 已经挂过了就直接返回（原版同样先 get_super）
        let existing = get_super(dev);
        if existing != NIL {
            return existing;
        }
        let n = get_empty_super();
        if n == NIL {
            pr_warn!("VFS: getting too many super blocks");
            return NIL;
        }
        // 一条裸指针写三个字段，见 [`sb_ptr`]：连着三次 `sb(n)` 是三条
        // 重叠的 `&mut`，整体赋值有可能盖掉后两个字段赋值。
        let p = sb_ptr(n);
        *p = SuperBlock::new();
        (*p).s_dev = dev;
        (*p).s_flags = flags;
        // 临时：写完立刻读回。读回来不是刚写的值，说明问题在这次写本身
        // （编译器/别名），而不是在后面某处把表写坏了。
        {
            let back = core::ptr::read_volatile(core::ptr::addr_of!((*p).s_dev));
            if back != dev {
                pr_warn!("DBG read_super: wrote s_dev={:#06x} but read back {:#06x} (slot {})",
                         dev, back, n);
                panic!("SUPER_AREA write lost");
            }
        }

        // 先试 ext4，再试 minix
        if !super::ext4::ops::read_super(n, silent) {
            if !super::minix::read_super(n, silent) {
                (*sb_ptr(n)).s_dev = 0;
                return NIL;
            }
        }
        n
    }
}

/// 回写脏超级块。对应原版 `sync_supers()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sync_supers(dev: u16) {
    // SAFETY: 契约转交。
    unsafe {
        for n in 0..NR_SUPER {
            if (*sb_ptr(n)).s_dev == 0 {
                continue;
            }
            if dev != 0 && (*sb_ptr(n)).s_dev != dev {
                continue;
            }
            wait_on_super(n);
            // 一条语句里两次 `sb(n)` = 两条重叠的 `&mut`（noalias UB）。
            // 先一次取完再判断。
            let (dev, dirt) = { let p = sb(n); (p.s_dev, p.s_dirt) };
            if dev == 0 || !dirt {
                continue;
            }
            // 原版是 sb->s_op->write_super(sb)
            super::minix::write_super(n);
        }
    }
}

/// 释放一个超级块。对应原版 `put_super()`。
///
/// # Safety
/// 只能在进程上下文调用。调用前该文件系统上必须没有在用的 inode。
pub unsafe fn put_super(dev: u16) {
    // SAFETY: 契约转交。
    unsafe {
        if dev == root_dev() {
            pr_warn!("VFS: Root device {:#x}: prepare for armageddon", dev);
            return;
        }
        let n = get_super(dev);
        if n == NIL {
            return;
        }
        if (*sb_ptr(n)).s_covered != NIL {
            pr_warn!("VFS: Mounted device {:#x} being removed", dev);
            return;
        }
        // 原版 sb->s_op->put_super(sb)
        super::minix::put_super(n);
    }
}

/// 挂载根文件系统。对应原版 `mount_root()`。
///
/// 原版的关键一步是那句带注释的 `inode->i_count += 3;`
/// （"NOTE! it is logically used 4 times, not 1"）：根 inode 同时被
/// `sb->s_mounted`、`sb->s_covered`、`current->pwd`、`current->root`
/// 四处引用。照搬。
///
/// 原版还会在根设备是软驱时提示插盘并 `wait_for_keypress()`；
/// 我们的根设备是 ramdisk，没有这一步。
///
/// # Safety
/// 启动期调用一次，需在 `fs::init()` 与驱动 `init()` 之后。
pub unsafe fn mount_root(dev: u16, flags: u64) -> bool {
    unsafe {
        *core::ptr::addr_of_mut!(ROOT_DEV) = dev;
        *core::ptr::addr_of_mut!(ROOT_MOUNTFLAGS) = flags;

        let n = read_super(dev, flags, false);
        if n == NIL {
            return false;
        }
        let root = (*sb_ptr(n)).s_mounted;
        if root == NIL {
            return false;
        }
        // 见函数文档：逻辑上被引用 4 次
        (*inode::inode_ptr(root)).i_count += 3;
        // s_covered 表示「被这个文件系统盖住的父文件系统目录」，根文件
        // 系统没有父文件系统，必须保持 NIL（SuperBlock::new() 的默认值）——
        // 之前这里误写成 root，把「盖住的目录」和「自己的根」搞混了。
        (*sb_ptr(n)).s_flags = flags;

        set_root_inode(root);

        check_mounted(n);

        pr_info!(
            "VFS: Mounted root{}.",
            if flags & MS_RDONLY != 0 { " readonly" } else { "" }
        );
        true
    }
}

/// 根 inode。原版存在 `current->root`；见 [`mount_root`] 的说明。
static mut ROOT_INODE: usize = NIL;
/// 当前工作目录 inode。原版 `current->pwd`。
static mut PWD_INODE: usize = NIL;

/// # Safety
/// 启动期调用（`mount_root` 里）。
unsafe fn set_root_inode(n: usize) {
    // SAFETY: 契约保证独占。
    unsafe {
        *core::ptr::addr_of_mut!(ROOT_INODE) = n;
        *core::ptr::addr_of_mut!(PWD_INODE) = n;
    }
}

/// 根 inode 的下标。
pub fn root_inode() -> usize {
    // SAFETY: 挂载后只读。
    unsafe { *core::ptr::addr_of!(ROOT_INODE) }
}

/// 当前工作目录的 inode 下标。
pub fn pwd_inode() -> usize {
    // SAFETY: 只读一个 usize；单核，`sys_chdir` 也在进程上下文改它。
    unsafe { *core::ptr::addr_of!(PWD_INODE) }
}

/// 换工作目录。对应原版 `sys_chdir` 里 `current->pwd = inode` 那一步。
///
/// # Safety
/// 只能在进程上下文调用。`n` 必须是已 `iget` 过的目录 inode。
pub unsafe fn set_pwd(n: usize) {
    // SAFETY: 契约转交；旧的 pwd 引用由调用方 iput。
    unsafe { *core::ptr::addr_of_mut!(PWD_INODE) = n }
}

/// 挂载一个文件系统。对应原版 `sys_mount()` 的正常路径 + `do_mount()`。
///
/// 原版检查顺序（照搬）：
/// 1. 非 root 用户 → `-EPERM`
/// 2. 设备文件必须是块设备（`S_ISBLK`）→ 否则 `-ENOTBLK`
/// 3. 挂载点必须是目录 → `-ENOTDIR`
/// 4. 挂载点不能已经被挂过、也不能是别人的挂载点 → `-EBUSY`
/// 5. 设备不能已经挂在别处 → `-EBUSY`
///
/// `dev_inode`/`dir_inode` 是已经 `iget` 过的 inode 下标（原版是
/// `namei()` 的结果）。成功后**不**释放它们：`s_covered` 持有 `dir_inode`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn do_mount(dev_inode: usize, dir_inode: usize, flags: u64) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        // 原版：`if (!suser()) return -EPERM`。我们还没有 uid 体系
        // （Task 里没有 uid 字段），所以这一步只留注释。
        // signal/exit 阶段补上 uid 后这里改成 `if !suser() { return -EPERM }`。

        let dev = {
            let di = inode::inode(dev_inode);
            if !mode::is_blk(di.i_mode) {
                return -(ENOTBLK as i64);
            }
            di.i_rdev
        };
        if crate::fs::devices::get_blkfops(major(dev)).is_none() {
            return -(ENODEV as i64);
        }
        {
            let d = inode::inode(dir_inode);
            if !mode::is_dir(d.i_mode) {
                return -(ENOTDIR as i64);
            }
            // 原版：`if (dir_i->i_count != 1 || dir_i->i_mount)`
            if d.i_count != 1 || d.i_mount != NIL {
                return -(EBUSY as i64);
            }
        }
        if get_super(dev) != NIL {
            return -(EBUSY as i64);
        }

        let n = read_super(dev, flags, false);
        if n == NIL {
            return -(EINVAL as i64);
        }
        let root = sb(n).s_mounted;
        if root == NIL {
            return -(EINVAL as i64);
        }
        // 建立双向的挂载关系（原版 do_mount 末尾那三行）
        (*sb_ptr(n)).s_covered = dir_inode;
        inode::inode(dir_inode).i_mount = root;
        0
    }
}

/// 卸载一个文件系统。对应原版 `do_umount()`。
///
/// 原版对根设备的特殊处理（改成只读重挂而不是真卸载）照搬。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn do_umount(dev: u16) -> i64 {
    unsafe {
        let n = get_super(dev);
        if n == NIL {
            return -(ENOENT as i64);
        }
        if dev == root_dev() {
            // get_super(dev) with dev==root_dev() already guarantees `n`
            // is the root superblock — no further check needed (and
            // s_covered is NIL for root, so there's nothing meaningful
            // to compare it against).
            buffer::fsync_dev(dev);
            sb(n).s_flags |= MS_RDONLY;
            sb(n).s_rd_only = true;
            pr_notice!("VFS: Root filesystem remounted read-only");
            return 0;
        }
        let covered = (*sb_ptr(n)).s_covered;
        if covered == NIL {
            return -(EINVAL as i64);
        }
        // 原版：s_mounted 的 i_count 必须是 1（只有超级块在引用它），
        // 否则还有进程在这个文件系统里
        let root = sb(n).s_mounted;
        if root != NIL && (*inode::inode_ptr(root)).i_count > 1 {
            return -(EBUSY as i64);
        }
        inode::inode(covered).i_mount = NIL;
        (*sb_ptr(n)).s_covered = NIL;
        inode::iput(covered);
        if root != NIL {
            sb(n).s_mounted = NIL;
            inode::iput(root);
        }
        buffer::fsync_dev(dev);
        super::minix::put_super(n);
        buffer::invalidate_buffers(dev);
        0
    }
}

/// 建立超级块表。对应原版 `mount_root()` 开头那句
/// `memset(super_blocks, 0, sizeof(super_blocks))`。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn init() {
    // SAFETY: 契约保证独占。
    unsafe {
        for n in 0..NR_SUPER {
            *sb(n) = SuperBlock::new();
        }
    }
    pr_info!("super: {} slots", NR_SUPER);
}

/// 消掉未使用告警：这几个 errno 在 uid 检查与 minix 校验补齐后会用到。
#[allow(dead_code)]
const _E: (i32, i32, u32, u32) = (EPERM, EINVAL, MEM_MAJOR, minor(0) as u32);
/// `FsType` 在 minix 的 `read_inode` 里设置；这里引入是为了文档链接。
#[allow(dead_code)]
const _F: FsType = FsType::Minix;
