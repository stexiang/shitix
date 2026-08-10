//! minix 的超级块与 inode 操作。对应 linux-1.0.9 的 `fs/minix/inode.c`。
//!
//! 实现的原版函数：`minix_read_super`、`minix_put_super`、
//! `minix_write_super`、`minix_commit_super`、`minix_read_inode`、
//! `minix_update_inode`、`minix_write_inode`、`minix_put_inode`、
//! `inode_bmap`/`block_bmap`/`minix_bmap`、
//! `inode_getblk`/`block_getblk`/`minix_getblk`/`minix_bread`。
//!
//! 不实现 `minix_statfs`（需要 `struct statfs` 与 `sys_statfs`）和
//! `minix_remount`。

use crate::fs::buffer::{self, BLOCK_SIZE, NIL, bh};
use crate::fs::inode::{self, FsType};
use crate::fs::super_block::{self, sb};
use crate::fs::{MS_RDONLY, mode};
use crate::sched;
use crate::{pr_info, pr_notice, pr_warn};

use super::{
    DIND_ZONE, DIRECT_ZONES, IND_ZONE, MINIX_INODES_PER_BLOCK, MINIX_ROOT_INO,
    MINIX_SUPER_MAGIC, MINIX_SUPER_MAGIC2, MINIX_VALID_FS, MINIX_ERROR_FS, MinixInode,
    MinixSuperBlock, ZONES_PER_BLOCK,
};

/// 单文件最大块数。对应原版那三处 `7+512+512*512`。
const MAX_FILE_BLOCKS: u32 =
    DIRECT_ZONES as u32 + ZONES_PER_BLOCK + ZONES_PER_BLOCK * ZONES_PER_BLOCK;

/// 从缓冲里按偏移读一个 `u16`（小端，同 i386 磁盘格式）。
///
/// 原版直接 `*(unsigned short *)(bh->b_data + off)`——i386 允许非对齐访问
/// 且原生小端，所以可以强转。我们显式 `from_le_bytes`：既不假设对齐
/// （缓冲基址是页对齐的，但偶数偏移之外没有保证），也把字节序写明。
#[inline]
fn read_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

/// 写一个 `u16`。
#[inline]
fn write_u16(data: &mut [u8], off: usize, v: u16) {
    assert!(off + 2 <= data.len(), "write_u16: off {} beyond {} bytes", off, data.len());
    data[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

/// 解析磁盘超级块。
fn parse_super(data: &[u8]) -> MinixSuperBlock {
    MinixSuperBlock {
        s_ninodes: read_u16(data, 0),
        s_nzones: read_u16(data, 2),
        s_imap_blocks: read_u16(data, 4),
        s_zmap_blocks: read_u16(data, 6),
        s_firstdatazone: read_u16(data, 8),
        s_log_zone_size: read_u16(data, 10),
        s_max_size: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
        s_magic: read_u16(data, 16),
        s_state: read_u16(data, 18),
    }
}

/// 读入超级块并挂上根 inode。对应原版 `minix_read_super()`。
///
/// 返回是否成功（原版返回 `struct super_block *` 或 NULL）。
///
/// # Safety
/// 只能在进程上下文调用。`n` 的 `s_dev`/`s_flags` 必须已填好。
pub unsafe fn read_super(n: usize, silent: bool) -> bool {
    // SAFETY: 契约转交。
    unsafe {
        let dev = sb(n).s_dev;
        super_block::lock_super(n);

        // 超级块在块 1（块 0 是引导块）
        let sbh = match buffer::bread(dev, 1, BLOCK_SIZE) {
            Some(b) => b,
            None => {
                super_block::unlock_super(n);
                pr_warn!("MINIX-fs: unable to read superblock");
                return false;
            }
        };
        let ms = parse_super(bh(sbh).data());

        // 整段用**同一个**裸指针写。分成两个 `{ let s = sb(n); ... }` 块
        // 是不行的：每个块各自产生一条 `&mut SuperBlock`，两条重叠的
        // `&mut` 带 noalias，LLVM 可以认为第二块的写与第一块无关而重排/
        // 丢弃。实测症状正是 `s_magic`/`s_ninodes`（第一块）都对，
        // `s_dirsize`/`s_namelen`（第二块）却留在 0，随后
        // `2 * dirsize` 乘法溢出或目录项偏移全错。
        let sp = super_block::sb_ptr(n);
        (*sp).s_sbh = sbh;
        (*sp).s_mount_state = ms.s_state;
        (*sp).s_blocksize = BLOCK_SIZE as u32;
        (*sp).s_blocksize_bits = buffer::BLOCK_SIZE_BITS as u8;
        (*sp).s_ninodes = ms.s_ninodes;
        (*sp).s_nzones = ms.s_nzones;
        (*sp).s_imap_blocks = ms.s_imap_blocks;
        (*sp).s_zmap_blocks = ms.s_zmap_blocks;
        (*sp).s_firstdatazone = ms.s_firstdatazone;
        (*sp).s_log_zone_size = ms.s_log_zone_size;
        (*sp).s_max_size = ms.s_max_size;
        (*sp).s_magic = ms.s_magic as u32;

        // 两种魔数决定名字长度与目录项大小（原版那个 if/else if/else）
        match ms.s_magic as u32 {
            MINIX_SUPER_MAGIC => {
                (*sp).s_dirsize = 16;
                (*sp).s_namelen = 14;
            }
            MINIX_SUPER_MAGIC2 => {
                (*sp).s_dirsize = 32;
                (*sp).s_namelen = 30;
            }
            _ => {
                super_block::unlock_super(n);
                buffer::brelse(sbh);
                sb(n).s_dev = 0;
                if !silent {
                    // 原版只打 "VFS: Can't find a minix filesystem on dev %04x."。
                    // 多带上魔数和 inode 数：读到的到底是垃圾还是别的块的内容，
                    // 这两个字段一眼能分出来。
                    pr_warn!("VFS: Can't find a minix filesystem on dev {:#06x} \
                              (magic={:#06x} ninodes={})",
                             dev, ms.s_magic, ms.s_ninodes);
                }
                return false;
            }
        }

        // 读入两张位图。原版按顺序 bread 进 s_imap[]/s_zmap[]，
        // 并在末尾校验读到的块数与 imap_blocks+zmap_blocks 相符。
        let mut block = 2u32;
        let slots = super::super_block_slots();
        let (nimap, nzmap) = { let p = sb(n); (p.s_imap_blocks as usize, p.s_zmap_blocks as usize) };
        if nimap > slots || nzmap > slots {
            super_block::unlock_super(n);
            buffer::brelse(sbh);
            sb(n).s_dev = 0;
            pr_warn!("MINIX-fs: bitmap too large ({} imap, {} zmap blocks)", nimap, nzmap);
            return false;
        }
        let mut ok = true;
        for i in 0..nimap {
            match buffer::bread(dev, block, BLOCK_SIZE) {
                Some(b) => {
                    sb(n).s_imap[i] = b;
                    block += 1;
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            for i in 0..nzmap {
                match buffer::bread(dev, block, BLOCK_SIZE) {
                    Some(b) => {
                        sb(n).s_zmap[i] = b;
                        block += 1;
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
        }
        if !ok || block != 2 + nimap as u32 + nzmap as u32 {
            for i in 0..slots {
                let (im, zm) = { let p = sb(n); (p.s_imap[i], p.s_zmap[i]) };
                buffer::brelse(im);
                buffer::brelse(zm);
                let p = sb(n);
                p.s_imap[i] = NIL;
                p.s_zmap[i] = NIL;
            }
            super_block::unlock_super(n);
            buffer::brelse(sbh);
            sb(n).s_dev = 0;
            pr_warn!("MINIX-fs: bad superblock or unable to read bitmaps");
            return false;
        }

        // 位图的第 0 位永远占用：不存在 inode 0，zone 位图的第 0 位
        // 对应 firstdatazone-1（见 bitmap.rs 模块文档的偏移说明）。
        // 原版这两句 set_bit 是必须的，否则 new_block/new_inode 会
        // 派出编号 0。
        // 先把下标取出来再取缓冲：`bh(sb(n).s_imap[0])` 会让 `sb()` 和
        // `bh()` 两条 `&mut` 同时活着（分别指向 SUPER_BLOCKS 和 BUFFERS），
        // 虽然对象不同，但把「读下标」和「用下标」写在一条表达式里会让
        // 求值顺序依赖优化，踩过一次同类问题（见 fs::buffer::buf_ptr）。
        let (im0, zm0) = { let p = sb(n); (p.s_imap[0], p.s_zmap[0]) };
        bh(im0).data_mut()[0] |= 1;
        buffer::mark_buffer_dirty(im0);
        bh(zm0).data_mut()[0] |= 1;
        buffer::mark_buffer_dirty(zm0);
        super_block::unlock_super(n);

        // 现在能读 inode 了，取根 inode
        let root = inode::iget(n, MINIX_ROOT_INO);
        if root == NIL {
            sb(n).s_dev = 0;
            buffer::brelse(sbh);
            pr_warn!("MINIX-fs: get root inode failed");
            return false;
        }
        sb(n).s_mounted = root;

        // 可写挂载时清 VALID_FS：表示「正在使用，没干净卸载」。
        // 卸载时 write_super 会写回。原版同样如此。
        if sb(n).s_flags & MS_RDONLY == 0 {
            let st = read_u16(bh(sbh).data(), 18) & !MINIX_VALID_FS;
            write_u16(bh(sbh).data_mut(), 18, st);
            buffer::mark_buffer_dirty(sbh);
            sb(n).s_dirt = true;
        }
        if sb(n).s_mount_state & MINIX_VALID_FS == 0 {
            pr_notice!("MINIX-fs: mounting unchecked file system, running fsck is recommended.");
        } else if sb(n).s_mount_state & MINIX_ERROR_FS != 0 {
            pr_notice!("MINIX-fs: mounting file system with errors, running fsck is recommended.");
        }
        pr_info!(
            "MINIX-fs: dev {:#06x}: {} inodes, {} zones, first data zone {}",
            dev,
            sb(n).s_ninodes,
            sb(n).s_nzones,
            sb(n).s_firstdatazone
        );
        true
    }
}

/// 回写超级块。对应原版 `minix_write_super()` + `minix_commit_super()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn write_super(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        if sb(n).s_flags & MS_RDONLY == 0 {
            let sbh = sb(n).s_sbh;
            if sbh != NIL {
                // 原版 commit_super：置回 VALID_FS 表示干净
                let st = read_u16(bh(sbh).data(), 18) | MINIX_VALID_FS;
                write_u16(bh(sbh).data_mut(), 18, st);
                buffer::mark_buffer_dirty(sbh);
            }
        }
        sb(n).s_dirt = false;
    }
}

/// 释放超级块与位图缓冲。对应原版 `minix_put_super()`。
///
/// # Safety
/// 只能在进程上下文调用；该文件系统上不能还有在用的 inode。
pub unsafe fn put_super(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        // 原版：干净卸载要把 s_state 恢复成挂载前的值
        if sb(n).s_flags & MS_RDONLY == 0 {
            let sbh = sb(n).s_sbh;
            let st = sb(n).s_mount_state;
            if sbh != NIL {
                write_u16(bh(sbh).data_mut(), 18, st);
                buffer::mark_buffer_dirty(sbh);
            }
        }
        let slots = super::super_block_slots();
        for i in 0..slots {
            // 同 read_super：先取下标再释放，别把 sb()/brelse 套在一条表达式里
            let (im, zm) = { let p = sb(n); (p.s_imap[i], p.s_zmap[i]) };
            buffer::brelse(im);
            buffer::brelse(zm);
            let p = sb(n);
            p.s_imap[i] = NIL;
            p.s_zmap[i] = NIL;
        }
        buffer::brelse(sb(n).s_sbh);
        sb(n).s_sbh = NIL;
        sb(n).s_dev = 0;
    }
}

// ---- inode 读写 ----

/// inode 号 `ino` 所在的块号与块内下标。对应原版
/// `block = 2 + imap_blocks + zmap_blocks + (ino-1)/MINIX_INODES_PER_BLOCK`。
///
/// 注意 `ino - 1`：inode 编号从 1 开始，而 inode 表从 0 号槽位开始装
/// 1 号 inode。少减这个 1 会整体错开一个 inode。
fn inode_location(sb_nr: usize, ino: u32) -> (u32, usize) {
    // SAFETY: sb_nr 由调用方保证有效；只读几个 u16。
    let (imap, zmap, ninodes, first_data) = unsafe {
        let p = sb(sb_nr);
        (
            p.s_imap_blocks as u32,
            p.s_zmap_blocks as u32,
            p.s_ninodes as u32,
            p.s_firstdatazone as u32,
        )
    };
    let block = 2 + imap + zmap + (ino - 1) / MINIX_INODES_PER_BLOCK;
    // 算出来的块必须落在 inode 表里：`2 + imap + zmap` 是表首块，
    // `s_firstdatazone` 是表后第一个数据块。越界说明超级块的
    // imap/zmap 字段被写坏了——那时这个块号会指到位图块上，读回来是
    // 一整块 0xFF，被 parse_inode 解成 `i_mode=0o177523`、`i_nlink=255`
    // 这种垃圾（bug-029 的直接成因）。在这里挡住，坏值本身就指明了源头；
    // 放到上层看只能看到「根 inode 的 mode 不对」，离原因很远。
    let table_first = 2 + imap + zmap;
    let table_last = table_first + ninodes.div_ceil(MINIX_INODES_PER_BLOCK);
    assert!(
        block >= table_first && block < table_last,
        "inode_location: ino {} -> block {} outside inode table [{}, {}) \
         (imap={} zmap={} ninodes={} firstdatazone={})",
        ino, block, table_first, table_last, imap, zmap, ninodes, first_data
    );
    let idx = ((ino - 1) % MINIX_INODES_PER_BLOCK) as usize;
    (block, idx)
}

/// 从缓冲里解析一个磁盘 inode。
///
/// 收裸指针而不是 `&[u8]`，并且逐字节 `read_volatile`——这是 bug-029 的修法。
///
/// 之前的版本收 `&[u8]`（来自 `BufferHead::data()`）。实测：驱动写进
/// 0xffdbc00 的是 `ed 41`（0o40755），紧挨着用 `read_volatile` 从同一个
/// 地址抓的快照也是 `ed 41`，两次取到的切片指针一模一样，而这个函数却
/// 解出 `i_mode=0o177523`（0xFF53，一整块 0xFF 的形态，即这个缓冲上一轮
/// 装位图块时的内容）。内存从头到尾都是对的。
///
/// 原因是 `&[u8]` 带 `noalias` + `readonly`：缓冲数据区实际是驱动用裸指针
/// memcpy 填的，LLVM 看不到那次写，于是认为切片存活期间这段内存不会变，
/// 把上一次读同一个缓冲的载入结果沿用了下来。`compiler_fence` 挡不住
/// （它只约束原子操作），空 `asm!` 的 memory clobber 也挡不住（引用是在
/// 屏障之后新建的，`readonly` 推断照样成立）。唯一可靠的做法是根本不建
/// 这个引用，直接 volatile 读。
///
/// # Safety
/// `base` 必须指向一个至少 `MINIX_INODE_SIZE * (idx + 1)` 字节的缓冲数据区，
/// 且当前没有 I/O 在改它。
unsafe fn parse_inode(base: *const u8, len: usize, idx: usize) -> MinixInode {
    let o = idx * super::MINIX_INODE_SIZE;
    assert!(
        o + super::MINIX_INODE_SIZE <= len,
        "parse_inode: idx {} out of block (buf {} bytes)",
        idx,
        len
    );
    // SAFETY: 契约保证 base + o + MINIX_INODE_SIZE 在缓冲内。
    unsafe {
        let p = base.add(o);
        let rd8 = |k: usize| core::ptr::read_volatile(p.add(k));
        let rd16 = |k: usize| u16::from_le_bytes([rd8(k), rd8(k + 1)]);
        let rd32 = |k: usize| {
            u32::from_le_bytes([rd8(k), rd8(k + 1), rd8(k + 2), rd8(k + 3)])
        };
        let mut zone = [0u16; 9];
        for (k, z) in zone.iter_mut().enumerate() {
            *z = rd16(14 + k * 2);
        }
        MinixInode {
            i_mode: rd16(0),
            i_uid: rd16(2),
            i_size: rd32(4),
            i_time: rd32(8),
            i_gid: rd8(12),
            i_nlinks: rd8(13),
            i_zone: zone,
        }
    }
}

/// 把一个磁盘 inode 写回缓冲。
///
/// `idx` 越界会 panic 并报出上下文：那说明调用方算出的 inode 号与
/// 超级块里的 `s_ninodes`/`s_imap_blocks` 不自洽，静默截断的话会把别的
/// inode 覆盖掉，比崩掉难查得多。
fn store_inode(data: &mut [u8], idx: usize, raw: &MinixInode) {
    let o = idx * super::MINIX_INODE_SIZE;
    assert!(
        o + super::MINIX_INODE_SIZE <= data.len(),
        "store_inode: idx {} out of block (buf {} bytes)",
        idx,
        data.len()
    );
    assert!(idx < super::MINIX_INODES_PER_BLOCK as usize, "store_inode: idx {} >= per-block", idx);
    write_u16(data, o, raw.i_mode);
    write_u16(data, o + 2, raw.i_uid);
    data[o + 4..o + 8].copy_from_slice(&raw.i_size.to_le_bytes());
    data[o + 8..o + 12].copy_from_slice(&raw.i_time.to_le_bytes());
    data[o + 12] = raw.i_gid;
    data[o + 13] = raw.i_nlinks;
    for (k, &z) in raw.i_zone.iter().enumerate() {
        write_u16(data, o + 14 + k * 2, z);
    }
}

/// 从磁盘读入 inode 内容。对应原版 `minix_read_inode()`。
///
/// 注意原版对设备文件的特殊处理：`S_ISCHR`/`S_ISBLK` 时 `i_zone[0]`
/// 不是块号而是设备号（存进 `i_rdev`），其余 8 项没有意义。
/// 混淆这两者会把设备号当块号去读盘。照搬。
///
/// # Safety
/// 只能在进程上下文调用。`n` 的 `i_sb`/`i_ino`/`i_dev` 必须已填好。
pub unsafe fn read_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let (sb_nr, ino, dev) = {
            let i = inode::inode(n);
            i.i_op = FsType::None;
            i.i_mode = 0;
            (i.i_sb, i.i_ino, i.i_dev)
        };
        if sb_nr == NIL {
            return;
        }
        if ino == 0 || ino >= sb(sb_nr).s_ninodes as u32 {
            pr_warn!("Bad inode number on dev {:#06x}: {} is out of range", dev, ino);
            return;
        }
        let (block, idx) = inode_location(sb_nr, ino);
        let b = match buffer::bread(dev, block, BLOCK_SIZE) {
            Some(b) => b,
            None => {
                pr_warn!("Major problem: unable to read inode from dev {:#06x}", dev);
                return;
            }
        };
        let raw = {
            let (p, len) = { let h = bh(b); (h.b_data as *const u8, h.b_size) };
            parse_inode(p, len, idx)
        };
        buffer::brelse(b);

        let i = inode::inode(n);
        i.i_mode = raw.i_mode;
        i.i_uid = raw.i_uid;
        i.i_gid = raw.i_gid as u16;
        i.i_nlink = raw.i_nlinks as u16;
        i.i_size = raw.i_size;
        // v1 只有一个时间戳，三个都填它（同原版）
        i.i_mtime = raw.i_time;
        i.i_atime = raw.i_time;
        i.i_ctime = raw.i_time;
        i.i_blksize = BLOCK_SIZE as u32;
        i.in_use = true;

        if mode::is_chr(raw.i_mode) || mode::is_blk(raw.i_mode) {
            // 见函数文档：设备文件的 i_zone[0] 是设备号
            i.i_rdev = raw.i_zone[0];
            i.data = [0; 9];
        } else {
            i.data = raw.i_zone;
        }

        // 原版按类型选 i_op（那一串 if/else if）
        i.i_op = if mode::is_reg(raw.i_mode) || mode::is_dir(raw.i_mode) {
            FsType::Minix
        } else if mode::is_chr(raw.i_mode) {
            FsType::Chr
        } else if mode::is_blk(raw.i_mode) {
            FsType::Blk
        } else {
            // 符号链接与 FIFO 没移植（见 minix/mod.rs 文档），
            // 保持 FsType::None —— open 会因此返回 -EINVAL 而不是
            // 走进一条半实现的路径。
            FsType::None
        };
    }
}

/// 把 inode 写回磁盘。对应原版 `minix_update_inode()` + `minix_write_inode()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn write_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let (sb_nr, ino, dev) = {
            let i = inode::inode(n);
            (i.i_sb, i.i_ino, i.i_dev)
        };
        if sb_nr == NIL {
            return;
        }
        if ino == 0 || ino >= sb(sb_nr).s_ninodes as u32 {
            pr_warn!("Bad inode number on dev {:#06x}: {} is out of range", dev, ino);
            inode::inode(n).i_dirt = false;
            return;
        }
        let (block, idx) = inode_location(sb_nr, ino);
        let b = match buffer::bread(dev, block, BLOCK_SIZE) {
            Some(b) => b,
            None => {
                pr_warn!("unable to read i-node block");
                inode::inode(n).i_dirt = false;
                return;
            }
        };

        let raw = {
            let i = inode::inode(n);
            let mut zone = [0u16; 9];
            if mode::is_chr(i.i_mode) || mode::is_blk(i.i_mode) {
                zone[0] = i.i_rdev;
            } else {
                zone = i.data;
            }
            MinixInode {
                i_mode: i.i_mode,
                i_uid: i.i_uid,
                i_size: i.i_size,
                i_time: i.i_mtime,
                i_gid: i.i_gid as u8,
                i_nlinks: i.i_nlink as u8,
                i_zone: zone,
            }
        };
        store_inode(bh(b).data_mut(), idx, &raw);
        buffer::mark_buffer_dirty(b);
        buffer::brelse(b);
        inode::inode(n).i_dirt = false;
    }
}

/// inode 引用归零时的处理。对应原版 `minix_put_inode()`。
///
/// 链接数还在就什么都不做；归零说明文件已被 unlink 且最后一个打开它的
/// 进程也关了，这时才真正释放数据块并回收 inode 位图位。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn put_inode(n: usize) {
    // SAFETY: 契约转交。
    unsafe {
        if (*inode::inode_ptr(n)).i_nlink != 0 {
            return;
        }
        inode::inode(n).i_size = 0;
        super::truncate::truncate(n);
        super::bitmap::free_inode(n);
    }
}

// ---- 块映射（bmap / getblk）----

/// 读 `i_zone[nr]`。对应原版 `inode_bmap()`。
///
/// # Safety
/// `n` 是有效 inode 下标，`nr < 9`。
#[inline]
unsafe fn inode_bmap(n: usize, nr: usize) -> u32 {
    // SAFETY: 契约转交。
    unsafe { inode::inode(n).data[nr] as u32 }
}

/// 读一个间接块里的第 `nr` 项。对应原版 `block_bmap()`。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn block_bmap(dev: u16, block: u32, nr: u32) -> u32 {
    if block == 0 {
        return 0;
    }
    // SAFETY: 契约转交。
    unsafe {
        let b = match buffer::bread(dev, block, BLOCK_SIZE) {
            Some(b) => b,
            None => return 0,
        };
        let v = read_u16(bh(b).data(), nr as usize * 2) as u32;
        buffer::brelse(b);
        v
    }
}

/// 文件内块号 → 设备块号。对应原版 `minix_bmap()`。
/// 返回 0 表示这个位置是空洞（或出错）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn bmap(n: usize, block: u32) -> u32 {
    // SAFETY: 契约转交。
    unsafe {
        if block >= MAX_FILE_BLOCKS {
            pr_warn!("minix_bmap: block>big");
            return 0;
        }
        let dev = inode::inode(n).i_dev;
        if block < DIRECT_ZONES as u32 {
            return inode_bmap(n, block as usize);
        }
        let block = block - DIRECT_ZONES as u32;
        if block < ZONES_PER_BLOCK {
            let ind = inode_bmap(n, IND_ZONE);
            if ind == 0 {
                return 0;
            }
            return block_bmap(dev, ind, block);
        }
        let block = block - ZONES_PER_BLOCK;
        let dind = inode_bmap(n, DIND_ZONE);
        if dind == 0 {
            return 0;
        }
        let mid = block_bmap(dev, dind, block / ZONES_PER_BLOCK);
        if mid == 0 {
            return 0;
        }
        block_bmap(dev, mid, block % ZONES_PER_BLOCK)
    }
}

// ---- 带分配的块查找（原版 inode_getblk / block_getblk / minix_getblk）----

/// 查 `i_zone[nr]`，`create` 时按需分配。对应原版 `inode_getblk()`。
///
/// 返回缓冲下标，[`NIL`] 表示失败。
///
/// 原版那个 `repeat:` 循环处理的是「`getblk` 会睡，睡醒后 `*p` 可能
/// 已经被别人填上了」这条竞态：所以分配完之后要再查一次 `*p`，
/// 非空就说明别人抢先了，把自己刚分的块还掉、用别人那个。照搬。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn inode_getblk(n: usize, nr: usize, create: bool) -> usize {
    // SAFETY: 契约转交。
    unsafe {
        let dev = inode::inode(n).i_dev;
        let sb_nr = inode::inode(n).i_sb;
        loop {
            let tmp = inode::inode(n).data[nr];
            if tmp != 0 {
                let result = match buffer::getblk(dev, tmp as u32, BLOCK_SIZE) {
                    Some(b) => b,
                    None => return NIL,
                };
                // 睡过之后 zone 号还是原来那个？是就用它
                if inode::inode(n).data[nr] == tmp {
                    return result;
                }
                buffer::brelse(result);
                continue;
            }
            if !create {
                return NIL;
            }
            let tmp = super::new_block(sb_nr);
            if tmp == 0 {
                return NIL;
            }
            let result = match buffer::getblk(dev, tmp, BLOCK_SIZE) {
                Some(b) => b,
                None => {
                    super::free_block(sb_nr, tmp);
                    return NIL;
                }
            };
            // 见函数文档：别人抢先填上了就用它的，还掉自己这块
            if inode::inode(n).data[nr] != 0 {
                super::free_block(sb_nr, tmp);
                buffer::brelse(result);
                continue;
            }
            inode::inode(n).data[nr] = tmp as u16;
            let i = inode::inode(n);
            i.i_ctime = sched::current_time();
            i.i_dirt = true;
            return result;
        }
    }
}

/// 在间接块 `ind_bh` 的第 `nr` 项上做同样的事。对应原版 `block_getblk()`。
///
/// **消耗 `ind_bh`**：无论成功失败都会 `brelse` 它（原版也是这个约定，
/// 这让 `minix_getblk` 能写成 `bh = block_getblk(inode, bh, ...)` 的链式
/// 调用而不泄漏引用）。
///
/// # Safety
/// 只能在进程上下文调用。`ind_bh` 必须是调用方持有的缓冲引用或 [`NIL`]。
unsafe fn block_getblk(n: usize, ind_bh: usize, nr: u32, create: bool) -> usize {
    if ind_bh == NIL {
        return NIL;
    }
    // SAFETY: 契约转交。
    unsafe {
        // 间接块本身可能还没读进来（getblk 只保证「有这个缓冲」）
        if !bh(ind_bh).b_uptodate {
            crate::drivers::block::ll_rw_block(crate::fs::READ, &mut [ind_bh]);
            buffer::wait_on_buffer(ind_bh);
            if !bh(ind_bh).b_uptodate {
                buffer::brelse(ind_bh);
                return NIL;
            }
        }
        let off = (nr * 2) as usize;
        if off + 2 > BLOCK_SIZE {
            buffer::brelse(ind_bh);
            return NIL;
        }
        let dev = bh(ind_bh).b_dev;
        let sb_nr = inode::inode(n).i_sb;

        loop {
            let tmp = read_u16(bh(ind_bh).data(), off);
            if tmp != 0 {
                let result = match buffer::getblk(dev, tmp as u32, BLOCK_SIZE) {
                    Some(b) => b,
                    None => {
                        buffer::brelse(ind_bh);
                        return NIL;
                    }
                };
                if read_u16(bh(ind_bh).data(), off) == tmp {
                    buffer::brelse(ind_bh);
                    return result;
                }
                buffer::brelse(result);
                continue;
            }
            if !create {
                buffer::brelse(ind_bh);
                return NIL;
            }
            let tmp = super::new_block(sb_nr);
            if tmp == 0 {
                buffer::brelse(ind_bh);
                return NIL;
            }
            let result = match buffer::getblk(dev, tmp, BLOCK_SIZE) {
                Some(b) => b,
                None => {
                    super::free_block(sb_nr, tmp);
                    buffer::brelse(ind_bh);
                    return NIL;
                }
            };
            if read_u16(bh(ind_bh).data(), off) != 0 {
                super::free_block(sb_nr, tmp);
                buffer::brelse(result);
                continue;
            }
            write_u16(bh(ind_bh).data_mut(), off, tmp as u16);
            buffer::mark_buffer_dirty(ind_bh);
            buffer::brelse(ind_bh);
            return result;
        }
    }
}

/// 取文件第 `block` 个逻辑块的缓冲，`create` 时按需分配（含间接块）。
/// 对应原版 `minix_getblk()`。
///
/// 返回缓冲下标或 [`NIL`]。内容**不保证**有效——要有效用 [`minix_bread`]。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn get_block(n: usize, block: u32, create: bool) -> usize {
    // SAFETY: 契约转交。
    unsafe {
        if block >= MAX_FILE_BLOCKS {
            pr_warn!("minix_getblk: block>big");
            return NIL;
        }
        if block < DIRECT_ZONES as u32 {
            return inode_getblk(n, block as usize, create);
        }
        let block = block - DIRECT_ZONES as u32;
        if block < ZONES_PER_BLOCK {
            let ind = inode_getblk(n, IND_ZONE, create);
            return block_getblk(n, ind, block, create);
        }
        let block = block - ZONES_PER_BLOCK;
        let dind = inode_getblk(n, DIND_ZONE, create);
        let mid = block_getblk(n, dind, block / ZONES_PER_BLOCK, create);
        block_getblk(n, mid, block % ZONES_PER_BLOCK, create)
    }
}

/// 取文件第 `block` 个逻辑块，内容保证有效。对应原版 `minix_bread()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn minix_bread(n: usize, block: u32, create: bool) -> usize {
    // SAFETY: 契约转交。
    unsafe {
        let b = get_block(n, block, create);
        if b == NIL || bh(b).b_uptodate {
            return b;
        }
        crate::drivers::block::ll_rw_block(crate::fs::READ, &mut [b]);
        buffer::wait_on_buffer(b);
        if bh(b).b_uptodate {
            return b;
        }
        buffer::brelse(b);
        NIL
    }
}

/// 消掉未使用告警。
#[allow(dead_code)]
const _M: (u16, u32) = (MINIX_ERROR_FS, MINIX_ROOT_INO);
