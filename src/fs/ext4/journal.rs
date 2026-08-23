//! JBD2 日志重放（journal replay）。
//!
//! 挂载时如果 ext4 超级块的 `s_feature_compat` 带 `HAS_JOURNAL`（0x4）
//! 且 `s_feature_incompat` 带 `RECOVER`（0x4，表示上次卸载不干净、
//! 日志里有未进盘的事务），把 journal inode（默认 8）里的已完成事务
//! （descriptor 之上有 commit 收尾的）重放回文件系统块。
//!
//! 流程（对应 jbd2 的 `journal_recover`）：
//!
//! 1. 读 journal inode 的原始 i_block（extent 树或经典指针）
//! 2. 映射 journal 逻辑块 → 文件系统物理块（fs 块单位）
//! 3. 读 journal superblock（第 0 块，全部字段大端）
//! 4. 从 `s_start` 起顺序扫描事务：
//!    - DESCRIPTOR：收集 (fs 块号, journal 数据块号) 对
//!    - REVOKE：收集该事务内被撤销的 fs 块（重放时跳过）
//!    - COMMIT：事务完整 → 应用收集的对（先读 journal 数据块、
//!      再逐 1024 字节子块写回 fs 块）
//!    - 没有 COMMIT → 半截事务，丢弃并停止
//! 5. 清 fs 超级块 RECOVER 标志、journal superblock s_start=0，
//!    sync_dev 落盘

use super::super::buffer;

/// FS feature 位
const FS_COMPAT_JOURNAL: u32 = 0x0004;
const FS_INCOMPAT_RECOVER: u32 = 0x0004;

/// JBD2 常量
const JBD2_MAGIC: u32 = 0xC03B_3998;
const BT_DESCRIPTOR: u32 = 1;
const BT_COMMIT: u32 = 2;
const BT_REVOKE: u32 = 5;
const JBD2_INCOMPAT_64BIT: u32 = 0x02;
const JBD2_INCOMPAT_CSUM_V3: u32 = 0x10;

/// tag 标志
const FLAG_SAME_UUID: u16 = 2;
const FLAG_LAST: u16 = 8;

/// 单个事务最多收集的 (fs块, journal块) 对 / 撤销项
const MAX_PAIRS: usize = 512;
const MAX_REVOKES: usize = 128;

/// 扫描用的临时页（4KB，fs 块 ≤ 4096 都装得下）
struct Scratch {
    page: usize,
}

impl Scratch {
    fn new() -> Option<Scratch> {
        let page = crate::mm::get_free_page();
        if page == 0 {
            return None;
        }
        Some(Scratch { page })
    }
    fn buf(&self) -> &'static mut [u8] {
        // SAFETY: page 是本对象持有的分配页。
        unsafe { core::slice::from_raw_parts_mut(self.page as *mut u8, 4096) }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        crate::mm::free_page(self.page);
    }
}

fn be16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}
fn be32(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
fn le32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// 读一个 fs 块（可能跨多个 1024 子块）到 dst（≥fs_bs 字节）。
fn read_fs_block(dev: u16, fs_blk: u64, fs_bs: usize, dst: &mut [u8]) -> bool {
    let scale = fs_bs / 1024;
    for k in 0..scale {
        // SAFETY: 缓冲层契约。
        let bn = unsafe { buffer::bread(dev, (fs_blk as u32) * scale as u32 + k as u32, 1024) };
        let Some(bn) = bn else { return false };
        // SAFETY: bn 有效。
        let data = unsafe { buffer::bh(bn).data() };
        let take = core::cmp::min(1024, dst.len() - k * 1024);
        dst[k * 1024..k * 1024 + take].copy_from_slice(&data[..take]);
        unsafe { buffer::brelse(bn) };
    }
    true
}

/// 把 src（≥fs_bs 字节）写进一个 fs 块。不存在的子块指针不动。
fn write_fs_block(dev: u16, fs_blk: u64, fs_bs: usize, src: &[u8]) {
    let scale = fs_bs / 1024;
    for k in 0..scale {
        // SAFETY: 缓冲层契约；先 read 拿块再整段覆盖。
        let bn = unsafe { buffer::bread(dev, (fs_blk as u32) * scale as u32 + k as u32, 1024) };
        let Some(bn) = bn else { continue };
        // SAFETY: bn 有效。
        let data = unsafe { buffer::bh(bn).data_mut() };
        let take = core::cmp::min(1024, src.len() - k * 1024);
        data[..take].copy_from_slice(&src[k * 1024..k * 1024 + take]);
        unsafe {
            buffer::mark_buffer_dirty(bn);
            buffer::brelse(bn);
        }
    }
}

/// journal inode 的块映射（extent 树 / 经典指针）。
fn map_journal_block(ib: &[u8; 60], lblock: u32, dev: u16, fs_bs: usize) -> Option<u64> {
    let uses_ext = u16::from_le_bytes([ib[0], ib[1]]) == 0xF30A;
    if uses_ext {
        return extent_lookup(ib, lblock, dev, fs_bs).map(|p| p);
    }
    classic_lookup(ib, lblock, dev, fs_bs)
}

/// extent 映射（照搬 ops.rs 里的实现，独立一份避免改公开性）
fn extent_lookup(ib: &[u8], lblock: u32, dev: u16, fs_bs: usize) -> Option<u64> {
    let root = super::extent::ExtentNode::parse(ib)?;
    if root.header().is_leaf() {
        let e = root.lookup_leaf(lblock)?;
        return Some(e.ee_start() + (lblock as u64 - e.ee_block() as u64));
    }
    let mut node_buf = [0u8; 60];
    node_buf[..ib.len().min(60)].copy_from_slice(&ib[..ib.len().min(60)]);
    for _ in 0..5 {
        let node = super::extent::ExtentNode::parse(&node_buf[..])?;
        let child_blk = node.lookup_index(lblock)?;
        // SAFETY: 缓冲层契约。
        let b = unsafe { buffer::bread(dev, child_blk as u32, fs_bs) }?;
        let data = unsafe { buffer::bh(b).data() };
        let take = core::cmp::min(data.len(), node_buf.len());
        node_buf[..take].copy_from_slice(&data[..take]);
        unsafe { buffer::brelse(b) };
        let child = super::extent::ExtentNode::parse(&node_buf[..])?;
        if child.header().is_leaf() {
            let e = child.lookup_leaf(lblock)?;
            return Some(e.ee_start() + (lblock as u64 - e.ee_block() as u64));
        }
    }
    None
}

/// 经典指针映射：12 直接 + 一级间接（journal inode 不会超过一级间接）。
fn classic_lookup(ib: &[u8; 60], lblock: u32, dev: u16, fs_bs: usize) -> Option<u64> {
    let rd32ib = |o: usize| -> u32 { le32(ib, o) };
    if lblock < 12 {
        let p = rd32ib((lblock as usize) * 4);
        return if p != 0 { Some(p as u64) } else { None };
    }
    let idx = lblock - 12;
    let ptrs = (fs_bs / 4) as u32;
    if idx >= ptrs {
        return None;
    }
    let ind = rd32ib(48);
    if ind == 0 {
        return None;
    }
    // SAFETY: 缓冲层契约。
    let scale = (fs_bs / 1024) as u32;
    let off_bytes = (idx as usize) * 4;
    let bn = unsafe {
        buffer::bread(dev, ind * scale + (off_bytes / 1024) as u32, 1024)
    }?;
    let data = unsafe { buffer::bh(bn).data() };
    let off = off_bytes % 1024;
    let p = le32(data, off);
    unsafe { buffer::brelse(bn) };
    if p == 0 { None } else { Some(p as u64) }
}

/// 事务收集：descriptor 里每个 tag 一对 (fs_blk, journal 数据 blk)。
struct Txn {
    pairs: [(u64, u64); MAX_PAIRS],
    np: usize,
    revokes: [u64; MAX_REVOKES],
    nr: usize,
    /// 有 descriptor 待提交
    pending: bool,
}

impl Txn {
    const fn new() -> Self {
        Txn {
            pairs: [(0, 0); MAX_PAIRS],
            np: 0,
            revokes: [0; MAX_REVOKES],
            nr: 0,
            pending: false,
        }
    }
    fn revoked(&self, fs_blk: u64) -> bool {
        self.revokes[..self.nr].contains(&fs_blk)
    }
}

/// 重放主入口。`sb_data` 是 1024 字节的 fs 超级块内容；
/// `inode_table_1024` 是组 0 inode 表（1024 块号）。
/// 返回 true = 处理完（无 recover 需求也算成功）。
pub unsafe fn recover(
    dev: u16,
    sb_data: &[u8],
    inode_table_1024: u64,
    inode_size: u32,
    fs_bs: usize,
    inodes_per_group: u32,
) -> bool {
    let compat = le32(sb_data, 92);
    let incompat = le32(sb_data, 96);
    if compat & FS_COMPAT_JOURNAL == 0 {
        // 无日志。如果 RECOVER 被置上，清掉它继续。
        if incompat & FS_INCOMPAT_RECOVER != 0 {
            clear_recover_flag(dev, sb_data);
        }
        return true;
    }
    if incompat & FS_INCOMPAT_RECOVER == 0 {
        return true; // 干净卸载，无需重放
    }
    crate::kprintln!("ext4: journal recovery needed, replaying");

    let j_inum = {
        let n = le32(sb_data, 224);
        if n == 0 { 8 } else { n }
    };

    // journal inode 肯定在组 0（inum 8）
    let group = ((j_inum - 1) / inodes_per_group) as u64;
    if group != 0 {
        crate::kprintln!("ext4: journal inode {} in group {}, cannot recover", j_inum, group);
        return clear_recover_flag(dev, sb_data);
    }
    let byte_off = ((j_inum - 1) as u64 % inodes_per_group as u64) * inode_size as u64;
    let iblk = inode_table_1024 + byte_off / 1024;
    // SAFETY: 缓冲层契约。
    let ibn = unsafe { buffer::bread(dev, iblk as u32, 1024) };
    let Some(ibn) = ibn else {
        return clear_recover_flag(dev, sb_data);
    };
    let idata = unsafe { buffer::bh(ibn).data() };
    let off = (byte_off % 1024) as usize;
    let ei = match super::inode::Ext4Inode::from_bytes(
        &idata[off..off + inode_size as usize],
    ) {
        Some(e) => e,
        None => {
            unsafe { buffer::brelse(ibn) };
            return clear_recover_flag(dev, sb_data);
        }
    };
    let ib = ei.i_block_raw();
    unsafe { buffer::brelse(ibn) };

    let Some(scratch) = Scratch::new() else {
        return clear_recover_flag(dev, sb_data);
    };
    let buf = scratch.buf();

    // journal superblock = journal 的第 0 块
    let Some(phys0) = map_journal_block(&ib, 0, dev, fs_bs) else {
        return clear_recover_flag(dev, sb_data);
    };
    if !read_fs_block(dev, phys0, fs_bs, buf) {
        return clear_recover_flag(dev, sb_data);
    }
    if be32(buf, 0) != JBD2_MAGIC {
        return clear_recover_flag(dev, sb_data);
    }
    let j_bsize = be32(buf, 12) as usize;
    let j_maxlen = be32(buf, 16) as u64;
    let j_first = be32(buf, 20) as u64;
    let j_start = be32(buf, 28) as u64;
    let j_features = be32(buf, 36);
    if j_bsize != fs_bs || j_maxlen == 0 {
        return clear_recover_flag(dev, sb_data);
    }
    if j_start == 0 {
        // 日志里没有待重放事务；清 RECOVER 即可
        return clear_recover_flag(dev, sb_data);
    }

    let tag_size = if j_features & JBD2_INCOMPAT_CSUM_V3 != 0 {
        16usize
    } else if j_features & JBD2_INCOMPAT_64BIT != 0 {
        12usize
    } else {
        8usize
    };

    let mut pos = j_start;
    let mut txn = Txn::new();
    let mut applied = 0u64;
    // 防止绕圈死循环
    let mut guard = j_maxlen;

    loop {
        if guard == 0 {
            break;
        }
        guard -= 1;
        let Some(jphys) = map_journal_block(&ib, pos as u32, dev, fs_bs) else {
            break;
        };
        if !read_fs_block(dev, jphys, fs_bs, buf) {
            break;
        }
        if be32(buf, 0) != JBD2_MAGIC {
            break; // 扫到非事务块，结束
        }
        let btype = be32(buf, 4);

        match btype {
            BT_DESCRIPTOR => {
                // tag 布局：t_blocknr(4) [+t_checksum(2)] t_flags(2) [+t_blocknr_high(4)]
                //   8 字节：blocknr(4) checksum(2) flags(2)
                //  12 字节（64BIT）：blocknr(4) checksum(2) flags(2) high(4)
                //  16 字节（CSUM_V3）：blocknr(4) flags(2) high(4) checksum(4) + 2 pad
                // 数据块紧随 descriptor，每个 tag 一块（SAME_UUID 不占数据块）。
                let mut data_idx = 0u64;
                let mut off = 12usize;
                while off + tag_size <= fs_bs && txn.np < MAX_PAIRS {
                    let fs_blk: u64 = match tag_size {
                        16 => (be32(buf, off) as u64) | (be32(buf, off + 6) as u64) << 32,
                        12 => (be32(buf, off) as u64) | (be32(buf, off + 8) as u64) << 32,
                        _ => be32(buf, off) as u64,
                    };
                    let flags: u16 = match tag_size {
                        16 => be16(buf, off + 4),
                        _ => be16(buf, off + 6),
                    };
                    let data_jpos = pos + 1 + data_idx;
                    if flags & FLAG_SAME_UUID == 0 {
                        data_idx += 1;
                    }
                    txn.pairs[txn.np] = (fs_blk, data_jpos);
                    txn.np += 1;
                    if flags & FLAG_LAST != 0 {
                        break;
                    }
                    off += tag_size;
                }
                txn.pending = true;
                pos = pos + 1 + data_idx;
                if pos >= j_maxlen {
                    pos = j_first;
                }
            }
            BT_COMMIT => {
                if txn.pending {
                    // 应用整笔事务
                    for i in 0..txn.np {
                        let (fs_blk, jpos) = txn.pairs[i];
                        if txn.revoked(fs_blk) {
                            continue;
                        }
                        if let Some(jphys) = map_journal_block(&ib, jpos as u32, dev, fs_bs) {
                            if read_fs_block(dev, jphys, fs_bs, buf) {
                                write_fs_block(dev, fs_blk, fs_bs, buf);
                            }
                        }
                    }
                    applied += txn.np as u64;
                }
                txn = Txn::new();
                pos += 1;
                if pos >= j_maxlen {
                    pos = j_first;
                }
            }
            BT_REVOKE => {
                // 撤销表：offset 8 起 u32 BE 块号（64bit 时 u64）
                let entry_size = if j_features & JBD2_INCOMPAT_64BIT != 0 { 12 } else { 4 };
                let cnt = ((fs_bs - 8) / entry_size).min(MAX_REVOKES - txn.nr);
                for i in 0..cnt {
                    let fs_blk = if entry_size == 12 {
                        (be32(buf, 8 + i * 12) as u64) | (be32(buf, 8 + i * 12 + 8) as u64) << 32
                    } else {
                        be32(buf, 8 + i * entry_size) as u64
                    };
                    if fs_blk == 0 {
                        break;
                    }
                    txn.revokes[txn.nr] = fs_blk;
                    txn.nr += 1;
                }
                pos += 1;
                if pos >= j_maxlen {
                    pos = j_first;
                }
            }
            _ => break,
        }
    }

    crate::kprintln!("ext4: journal replay done, {} blocks applied", applied);

    // journal superblock 写回 s_start=0
    if read_fs_block(dev, phys0, fs_bs, buf) {
        buf[28..32].fill(0); // s_start @ offset 28（BE）
        write_fs_block(dev, phys0, fs_bs, buf);
    }
    clear_recover_flag(dev, sb_data)
}

/// 清掉 fs 超级块的 RECOVER 标志并落盘。
fn clear_recover_flag(dev: u16, sb_data: &[u8]) -> bool {
    // SAFETY: 超级块在字节偏移 1024；1024 块号恒为 1。
    let bn = unsafe { buffer::bread(dev, 1, 1024) };
    let Some(bn) = bn else { return false };
    let r = unsafe {
        let data = buffer::bh(bn).data_mut();
        if data.len() >= 100 && sb_data.get(96..100) == data.get(96..100) {
            let incompat = u32::from_le_bytes([data[96], data[97], data[98], data[99]]);
            let cleared = incompat & !FS_INCOMPAT_RECOVER;
            data[96..100].copy_from_slice(&cleared.to_le_bytes());
            buffer::mark_buffer_dirty(bn);
        }
        buffer::brelse(bn);
        buffer::sync_dev(dev);
        true
    };
    r
}
