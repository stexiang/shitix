//! inode 与 zone 位图。对应 linux-1.0.9 的 `fs/minix/bitmap.c`。
//!
//! 位图的位序与原版的 `set_bit`/`clear_bit`/`find_first_zero` 内联汇编
//! 完全一致：第 `n` 位在字节 `n/8` 的第 `n%8` 位（最低位起）。
//! 原版用的是 386 的 `bts`/`btr`/`bsf` 指令，我们用普通的移位——
//! 位序相同（x86 的 `bt` 系列对内存操作数就是这个约定），所以造出来的
//! 位图与原版工具（`mkfs.minix`、`fsck.minix`）兼容。
//!
//! 一个重要的偏移约定（原版的坑，照搬）：**zone 位图的第 0 位对应
//! `s_firstdatazone - 1`**，也就是块号 `b` 的位号是
//! `b - s_firstdatazone + 1`。inode 位图则是第 `ino` 位对应 inode 号 `ino`，
//! 第 0 位永远置 1（不存在 inode 0）。这两个不一致的偏移是 minix 格式
//! 本身的历史遗留，弄错会让位图与实际占用错开一块。

use crate::fs::buffer::{self, NIL, bh};
use crate::fs::inode::{self, FsType};
use crate::fs::super_block::{self, sb};
use crate::pr_warn;

use super::{BITS_PER_BLOCK, MINIX_INODES_PER_BLOCK};

/// 读一位。对应原版内联汇编里的 `bt`。
#[inline]
fn test_bit(data: &[u8], n: u32) -> bool {
    let byte = (n / 8) as usize;
    if byte >= data.len() {
        return true; // 越界当已占用，比误报空闲安全
    }
    data[byte] & (1 << (n % 8)) != 0
}

/// 置一位，返回原来的值。对应原版 `set_bit()`（返回旧位）。
#[inline]
fn set_bit(data: &mut [u8], n: u32) -> bool {
    let byte = (n / 8) as usize;
    if byte >= data.len() {
        return true;
    }
    let mask = 1u8 << (n % 8);
    let old = data[byte] & mask != 0;
    data[byte] |= mask;
    old
}

/// 清一位，返回原来的值。对应原版 `clear_bit()`。
#[inline]
fn clear_bit(data: &mut [u8], n: u32) -> bool {
    let byte = (n / 8) as usize;
    if byte >= data.len() {
        return false;
    }
    let mask = 1u8 << (n % 8);
    let old = data[byte] & mask != 0;
    data[byte] &= !mask;
    old
}

/// 找第一个 0 位。对应原版 `find_first_zero()`，
/// 找不到返回 [`BITS_PER_BLOCK`]（原版返回 8192）。
fn find_first_zero(data: &[u8]) -> u32 {
    for (i, &b) in data.iter().enumerate() {
        if b != 0xff {
            // trailing_ones 就是这个字节里第一个 0 的位号
            return i as u32 * 8 + b.trailing_ones();
        }
    }
    BITS_PER_BLOCK
}

/// 分配一个数据块，返回块号，失败返回 0。
/// 对应原版 `minix_new_block()`。
///
/// 原版分配成功后会 `getblk` 拿到那一块并**清零**（`clear_block`）、
/// 置 `b_uptodate = 1`、`b_dirt = 1`。这一步不能省：新块的旧内容是
/// 上一个文件删掉留下的数据，不清零就是信息泄漏，而且 `b_uptodate`
/// 不置位的话下次 `bread` 会去磁盘读回那些垃圾。照搬。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。`sb_nr` 必须是有效超级块下标。
pub unsafe fn new_block(sb_nr: usize) -> u32 {
    if sb_nr == NIL {
        pr_warn!("trying to get new block from nonexistent device");
        return 0;
    }
    // SAFETY: 契约转交。
    unsafe {
        loop {
            // 沿 8 个 zone 位图块找第一个空闲位
            let mut i = 0usize;
            let mut j = BITS_PER_BLOCK;
            let mut map = NIL;
            while i < super::super_block_slots() {
                let b = sb(sb_nr).s_zmap[i];
                if b != NIL {
                    j = find_first_zero(bh(b).data());
                    if j < BITS_PER_BLOCK {
                        map = b;
                        break;
                    }
                }
                i += 1;
            }
            if map == NIL || j >= BITS_PER_BLOCK {
                return 0; // 磁盘满
            }
            if set_bit(bh(map).data_mut(), j) {
                // 原版：printk("new_block: bit already set") 然后 goto repeat
                pr_warn!("new_block: bit already set");
                continue;
            }
            buffer::mark_buffer_dirty(map);

            // 位号 → 块号。见模块文档里那个 +1 偏移
            let block = j + i as u32 * BITS_PER_BLOCK + sb(sb_nr).s_firstdatazone as u32 - 1;
            let (fdz, nz) = { let p = sb(sb_nr); (p.s_firstdatazone as u32, p.s_nzones as u32) };
            if block < fdz || block >= nz {
                return 0;
            }

            // 清零新块（见函数文档）
            let dev = sb(sb_nr).s_dev;
            let nb = match buffer::getblk(dev, block, crate::fs::BLOCK_SIZE) {
                Some(n) => n,
                None => {
                    pr_warn!("new_block: cannot get block");
                    return 0;
                }
            };
            bh(nb).data_mut().fill(0);
            bh(nb).b_uptodate = true;
            buffer::mark_buffer_dirty(nb);
            buffer::brelse(nb);
            return block;
        }
    }
}

/// 释放一个数据块。对应原版 `minix_free_block()`。
///
/// 原版先把这一块在缓冲缓存里的副本 `b_dirt = 0`：块已经不属于任何文件了，
/// 把它的旧内容写回磁盘毫无意义，反而会覆盖掉之后重新分配它的人写的数据。
/// 照搬。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn free_block(sb_nr: usize, block: u32) {
    if sb_nr == NIL {
        pr_warn!("trying to free block on nonexistent device");
        return;
    }
    // SAFETY: 契约转交。
    unsafe {
        let (fdz, nz) = { let p = sb(sb_nr); (p.s_firstdatazone as u32, p.s_nzones as u32) };
        if block < fdz || block >= nz {
            pr_warn!("trying to free block not in datazone");
            return;
        }
        let dev = sb(sb_nr).s_dev;
        // 丢掉缓存里的副本（见函数文档）
        if let Some(n) = buffer::get_hash_table(dev, block, crate::fs::BLOCK_SIZE) {
            bh(n).b_dirt = false;
            buffer::brelse(n);
        }

        let zone = block - sb(sb_nr).s_firstdatazone as u32 + 1;
        let bit = zone & (BITS_PER_BLOCK - 1);
        let idx = (zone / BITS_PER_BLOCK) as usize;
        if idx >= super::super_block_slots() {
            pr_warn!("minix_free_block: zone {} out of bitmap range", zone);
            return;
        }
        let map = sb(sb_nr).s_zmap[idx];
        if map == NIL {
            pr_warn!("minix_free_block: nonexistent bitmap buffer");
            return;
        }
        if !clear_bit(bh(map).data_mut(), bit) {
            pr_warn!("free_block ({:04x}:{}): bit already cleared", dev, block);
        }
        buffer::mark_buffer_dirty(map);
    }
}

/// 分配一个 inode。对应原版 `minix_new_inode()`。
///
/// 返回内存 inode 表的下标，[`NIL`] 表示失败。
///
/// # Safety
/// 只能在进程上下文调用。`dir` 是父目录的 inode 下标（用来继承
/// `i_gid` 与所在文件系统）。
pub unsafe fn new_inode(dir: usize) -> usize {
    if dir == NIL {
        return NIL;
    }
    // SAFETY: 契约转交。
    unsafe {
        let sb_nr = inode::inode(dir).i_sb;
        if sb_nr == NIL {
            return NIL;
        }
        let n = inode::get_empty_inode();
        if n == NIL {
            return NIL;
        }

        // 沿 8 个 inode 位图块找空闲位
        let mut i = 0usize;
        let mut j = BITS_PER_BLOCK;
        let mut map = NIL;
        while i < super::super_block_slots() {
            let b = sb(sb_nr).s_imap[i];
            if b != NIL {
                j = find_first_zero(bh(b).data());
                if j < BITS_PER_BLOCK {
                    map = b;
                    break;
                }
            }
            i += 1;
        }
        if map == NIL || j >= BITS_PER_BLOCK {
            inode::iput(n);
            return NIL;
        }
        if set_bit(bh(map).data_mut(), j) {
            // 原版注释：/* shouldn't happen */
            pr_warn!("new_inode: bit already set");
            inode::iput(n);
            return NIL;
        }
        buffer::mark_buffer_dirty(map);

        let ino = j + i as u32 * BITS_PER_BLOCK;
        if ino == 0 || ino >= sb(sb_nr).s_ninodes as u32 {
            inode::iput(n);
            return NIL;
        }

        let (dev, sflags, dir_gid, dir_mode) = {
            let d = inode::inode(dir);
            let (dv, fl) = { let p = sb(sb_nr); (p.s_dev, p.s_flags) };
            (dv, fl, d.i_gid, d.i_mode)
        };
        let now = crate::sched::current_time();
        let ni = inode::inode(n);
        ni.i_sb = sb_nr;
        ni.i_flags = sflags;
        ni.i_count = 1;
        ni.i_nlink = 1;
        ni.i_dev = dev;
        ni.i_ino = ino;
        // 原版 `inode->i_uid = current->euid`；我们还没有 uid 体系
        // （见 sched/task.rs 的字段取舍），统一用 0 = root。
        ni.i_uid = 0;
        // 原版：目录有 setgid 位就继承目录的 gid，否则用 current->egid
        ni.i_gid = if dir_mode & crate::fs::mode::S_ISGID != 0 { dir_gid } else { 0 };
        ni.i_mtime = now;
        ni.i_atime = now;
        ni.i_ctime = now;
        ni.i_dirt = true;
        ni.i_op = FsType::None; // 由调用方按 i_mode 设定
        ni.i_blksize = crate::fs::BLOCK_SIZE as u32;
        ni.in_use = true;
        n
    }
}

/// 释放一个 inode。对应原版 `minix_free_inode()`。
///
/// 原版那四条前置检查（有设备、`i_count == 1`、`i_nlink == 0`、
/// 有超级块）全部照搬：它们各自对应一类调用方 bug，静默放过会让
/// 位图和 inode 表悄悄不一致。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn free_inode(n: usize) {
    if n == NIL {
        return;
    }
    // SAFETY: 契约转交。
    unsafe {
        {
            let i = inode::inode(n);
            if i.i_dev == 0 {
                pr_warn!("free_inode: inode has no device");
                return;
            }
            if i.i_count != 1 {
                pr_warn!("free_inode: inode has count={}", i.i_count);
                return;
            }
            if i.i_nlink != 0 {
                pr_warn!("free_inode: inode has nlink={}", i.i_nlink);
                return;
            }
            if i.i_sb == NIL {
                pr_warn!("free_inode: inode on nonexistent device");
                return;
            }
        }
        let (sb_nr, ino) = {
            let i = inode::inode(n);
            (i.i_sb, i.i_ino)
        };
        if ino < 1 || ino >= sb(sb_nr).s_ninodes as u32 {
            pr_warn!("free_inode: inode 0 or nonexistent inode");
            return;
        }
        let idx = (ino / BITS_PER_BLOCK) as usize;
        if idx >= super::super_block_slots() {
            pr_warn!("free_inode: nonexistent imap in superblock");
            return;
        }
        let map = sb(sb_nr).s_imap[idx];
        if map == NIL {
            pr_warn!("free_inode: nonexistent imap in superblock");
            return;
        }
        // 原版顺序：先 clear_inode 再清位。反过来会让「位已空闲但 inode
        // 表里还残留旧内容」的窗口暴露给 iget。
        inode::clear_inode(n);
        if !clear_bit(bh(map).data_mut(), ino & (BITS_PER_BLOCK - 1)) {
            pr_warn!("free_inode: bit {} already cleared.", ino);
        }
        buffer::mark_buffer_dirty(map);
    }
}

/// 已用/总数统计。原版 `minix_statfs` 会算这个；自检用。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn count_free(sb_nr: usize, zmap: bool) -> u32 {
    // SAFETY: 契约转交；只读位图缓冲。
    unsafe {
        let mut free = 0u32;
        for i in 0..super::super_block_slots() {
            let b = { let p = sb(sb_nr); if zmap { p.s_zmap[i] } else { p.s_imap[i] } };
            if b == NIL {
                continue;
            }
            for &byte in bh(b).data() {
                free += byte.count_zeros();
            }
        }
        free
    }
}

/// 列出已分配的 zone 位号（调试/自检用，原版无）。最多写 `out` 那么多个，
/// 返回实际个数。
///
/// # Safety
/// 只能在进程上下文调用；只读位图缓冲。
pub unsafe fn allocated_zones(sb_nr: usize, out: &mut [u32]) -> usize {
    // SAFETY: 契约转交。
    unsafe {
        let mut k = 0usize;
        for i in 0..super::super_block_slots() {
            let b = sb(sb_nr).s_zmap[i];
            if b == NIL {
                continue;
            }
            let base = i as u32 * BITS_PER_BLOCK;
            for (byte_i, &byte) in bh(b).data().iter().enumerate() {
                if byte == 0 {
                    continue;
                }
                for bit in 0..8u32 {
                    if byte & (1 << bit) != 0 {
                        if k < out.len() {
                            out[k] = base + byte_i as u32 * 8 + bit;
                        }
                        k += 1;
                    }
                }
            }
        }
        k
    }
}

/// 消掉未使用告警：这两个在 `truncate`/`namei` 里通过 `super::` 路径用到。
#[allow(dead_code)]
const _X: (u32, usize) = (MINIX_INODES_PER_BLOCK, super_block::MINIX_I_MAP_SLOTS);
#[allow(dead_code)]
fn _test_bit_used(d: &[u8], n: u32) -> bool {
    test_bit(d, n)
}
