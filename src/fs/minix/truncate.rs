//! 文件截断。对应 linux-1.0.9 的 `fs/minix/truncate.c`。
//!
//! 只实现「截断到 0」以外也支持任意长度的完整语义（原版 `minix_truncate`
//! 就是按 `inode->i_size` 算出保留到第几块，其余全部释放）。
//!
//! 原版把释放分成三层函数：`trunc_direct`（直接块）、
//! `trunc_indirect`（一级间接）、`trunc_dindirect`（二级间接），
//! 每层都有那个 `repeat:` 循环处理「`brelse` 会睡，睡醒后
//! zone 号可能变了」的竞态，并且只在整个间接块都空了之后才释放
//! 间接块本身。这三层结构照搬。
//!
//! 原版每层末尾都有 `if (retry) goto repeat`，`retry` 在
//! 「块还被别人引用（`bh->b_count != 1`）」时置位——那是因为
//! 释放一个还被引用的块会让引用者写回到一个已经重新分配出去的块上。
//! 我们同样保留这个检查。

use crate::fs::buffer::{self, BLOCK_SIZE, bh};
use crate::fs::inode;
use crate::sched;

use super::{DIND_ZONE, DIRECT_ZONES, IND_ZONE, ZONES_PER_BLOCK};

/// 从缓冲读一个小端 u16。
#[inline]
fn read_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

/// 写一个小端 u16。
#[inline]
fn write_u16(data: &mut [u8], off: usize, v: u16) {
    data[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

/// 释放直接块里超出 `first` 的部分。对应原版 `trunc_direct()`。
///
/// 返回是否需要重试（原版的 `retry`）。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn trunc_direct(n: usize, first: u32) -> bool {
    // SAFETY: 契约转交。
    unsafe {
        let mut retry = false;
        let sb_nr = inode::inode(n).i_sb;
        let dev = inode::inode(n).i_dev;
        let start = first.min(DIRECT_ZONES as u32) as usize;
        for k in start..DIRECT_ZONES {
            let z = inode::inode(n).data[k];
            if z == 0 {
                continue;
            }
            // 原版：先看缓存里这块还有没有别人在用
            if let Some(b) = buffer::get_hash_table(dev, z as u32, BLOCK_SIZE) {
                let busy = (*buffer::buf_ptr(b)).b_count != 1;
                buffer::brelse(b);
                if busy {
                    retry = true;
                    continue;
                }
            }
            // 再查一次：brelse 可能睡过，zone 号可能已经变了
            if inode::inode(n).data[k] != z {
                retry = true;
                continue;
            }
            inode::inode(n).data[k] = 0;
            super::free_block(sb_nr, z as u32);
            inode::inode(n).i_dirt = true;
        }
        retry
    }
}

/// 释放一个一级间接块里超出 `first` 的项，块全空则释放它自己。
/// 对应原版 `trunc_indirect()`。
///
/// `zone_slot` 是 `i_zone` 里存这个间接块号的下标（一级间接是
/// [`IND_ZONE`]，二级间接的中间层则由调用方传缓冲里的偏移，
/// 见 [`trunc_dindirect`]）。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn trunc_indirect(n: usize, first: u32, ind_block: u32) -> (bool, bool) {
    // SAFETY: 契约转交。
    unsafe {
        let sb_nr = inode::inode(n).i_sb;
        let dev = inode::inode(n).i_dev;
        let ib = match buffer::bread(dev, ind_block, BLOCK_SIZE) {
            Some(b) => b,
            // 读不出来就当它空了：原版这里返回 0（不 retry），
            // 否则一个坏的间接块会让 truncate 永远转下去
            None => return (false, false),
        };
        let mut retry = false;
        let start = first.min(ZONES_PER_BLOCK);
        for k in start..ZONES_PER_BLOCK {
            let off = (k * 2) as usize;
            let z = read_u16(bh(ib).data(), off);
            if z == 0 {
                continue;
            }
            if let Some(b) = buffer::get_hash_table(dev, z as u32, BLOCK_SIZE) {
                let busy = (*buffer::buf_ptr(b)).b_count != 1;
                buffer::brelse(b);
                if busy {
                    retry = true;
                    continue;
                }
            }
            if read_u16(bh(ib).data(), off) != z {
                retry = true;
                continue;
            }
            write_u16(bh(ib).data_mut(), off, 0);
            buffer::mark_buffer_dirty(ib);
            super::free_block(sb_nr, z as u32);
        }

        // 整块都空了？空了才能释放间接块本身（原版那个 `for (i=0;i<512;i++)
        // if (ind->i_zone[i]) break;` 的检查）
        let mut all_zero = true;
        for k in 0..ZONES_PER_BLOCK {
            if read_u16(bh(ib).data(), (k * 2) as usize) != 0 {
                all_zero = false;
                break;
            }
        }
        let busy = (*buffer::buf_ptr(ib)).b_count != 1;
        buffer::brelse(ib);
        let freeable = all_zero && !busy && !retry;
        if all_zero && busy {
            retry = true;
        }
        (retry, freeable)
    }
}

/// 释放二级间接。对应原版 `trunc_dindirect()`。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn trunc_dindirect(n: usize, first: u32) -> bool {
    // SAFETY: 契约转交。
    unsafe {
        let dind = inode::inode(n).data[DIND_ZONE];
        if dind == 0 {
            return false;
        }
        let sb_nr = inode::inode(n).i_sb;
        let dev = inode::inode(n).i_dev;
        let db = match buffer::bread(dev, dind as u32, BLOCK_SIZE) {
            Some(b) => b,
            None => return false,
        };
        let mut retry = false;
        // first 是二级间接区域内的块号；除以扇出得到从第几个中间块开始
        let start_mid = first / ZONES_PER_BLOCK;
        for k in start_mid..ZONES_PER_BLOCK {
            let off = (k * 2) as usize;
            let mid = read_u16(bh(db).data(), off);
            if mid == 0 {
                continue;
            }
            // 这个中间块内部要从第几项开始截
            let inner_first = if k == start_mid { first % ZONES_PER_BLOCK } else { 0 };
            let (r, freeable) = trunc_indirect(n, inner_first, mid as u32);
            retry |= r;
            if freeable {
                write_u16(bh(db).data_mut(), off, 0);
                buffer::mark_buffer_dirty(db);
                super::free_block(sb_nr, mid as u32);
            }
        }

        let mut all_zero = true;
        for k in 0..ZONES_PER_BLOCK {
            if read_u16(bh(db).data(), (k * 2) as usize) != 0 {
                all_zero = false;
                break;
            }
        }
        let busy = (*buffer::buf_ptr(db)).b_count != 1;
        buffer::brelse(db);
        if all_zero && !busy && !retry {
            inode::inode(n).data[DIND_ZONE] = 0;
            super::free_block(sb_nr, dind as u32);
            inode::inode(n).i_dirt = true;
        } else if all_zero && busy {
            retry = true;
        }
        retry
    }
}

/// 把文件截断到 `i_size`。对应原版 `minix_truncate()`。
///
/// 调用方先设好 `inode.i_size`，这里释放超出部分的所有块。
///
/// 原版最外层是 `while (1) { ... if (!retry) break; current->counter = 0;
/// schedule(); }`：重试之前主动让出 CPU，让占着那些块的进程有机会
/// 放手。照搬——不让出的话在单核上就是活锁。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn truncate(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        // 保留到第几块（向上取整：最后一块的部分内容要留着）
        let size = inode::inode(n).i_size as u64;
        let keep = ((size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64) as u32;

        let mut rounds = 0;
        loop {
            let mut retry = trunc_direct(n, keep);

            // 一级间接
            let ind_first = keep.saturating_sub(DIRECT_ZONES as u32);
            let ind = inode::inode(n).data[IND_ZONE];
            if ind != 0 {
                let (r, freeable) = trunc_indirect(n, ind_first, ind as u32);
                retry |= r;
                if freeable {
                    let sb_nr = inode::inode(n).i_sb;
                    inode::inode(n).data[IND_ZONE] = 0;
                    super::free_block(sb_nr, ind as u32);
                    inode::inode(n).i_dirt = true;
                }
            }

            // 二级间接
            let dind_first = keep.saturating_sub(DIRECT_ZONES as u32 + ZONES_PER_BLOCK);
            retry |= trunc_dindirect(n, dind_first);

            if !retry {
                break;
            }
            rounds += 1;
            if rounds > 16 {
                // 原版没有这个上限（它信任 retry 最终会平息）。加一个是因为
                // 「缓冲被永久引用」这种 bug 在原版表现为 truncate 挂死，
                // 而挂死比留几个泄漏的块难诊断得多。
                crate::pr_warn!("minix_truncate: giving up after {} retries", rounds);
                break;
            }
            // 见函数文档：让出 CPU 再重试
            sched::yield_now();
        }

        let i = inode::inode(n);
        i.i_mtime = sched::current_time();
        i.i_ctime = i.i_mtime;
        i.i_dirt = true;
    }
}
