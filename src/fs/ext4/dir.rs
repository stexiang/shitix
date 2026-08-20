//! ext4 目录项
//!
//! 对应内核 `struct ext4_dir_entry_2`（`fs/ext4/ext4.h`）。ext2 原始的
//! `ext4_dir_entry` 用 16 位 `name_len`；开了 `INCOMPAT_FILETYPE` 之后
//! 高字节被拆出来当文件类型，于是变成 `u8 name_len` + `u8 file_type`。
//! LFS 上 `mkfs.ext4` 默认开 filetype，所以这里按 `_2` 布局解析。
//!
//! 磁盘布局（变长，8 字节头 + 名字，整条按 4 字节对齐）：
//! ```text
//! 0..4  inode     指向的 inode 号，0 = 该槽位已删除
//! 4..6  rec_len    本条记录总长（含头和填充），走到下一条要加这个值
//! 6     name_len   名字字节数
//! 7     file_type  EXT4_FT_*
//! 8..   name       不带结尾 NUL
//! ```

use super::file_type;

/// 目录项头的固定长度
pub const EXT4_DIR_ENTRY_HEADER_LEN: usize = 8;
/// 名字上限
pub const EXT4_NAME_LEN: usize = 255;

/// 一条已解析的目录项
#[derive(Debug, Clone, Copy)]
pub struct Ext4DirEntry {
    /// inode 号（0 表示空槽）
    pub inode: u32,
    /// 记录总长
    pub rec_len: u16,
    /// 名字长度
    pub name_len: u8,
    /// 文件类型（EXT4_FT_*）
    pub file_type: u8,
    /// 名字在所属缓冲里的字节偏移
    pub name_off: usize,
}

impl Ext4DirEntry {
    /// 是否是空槽（被删除的项）
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inode == 0
    }

    /// 文件类型名
    #[inline]
    pub fn type_name(&self) -> &'static str {
        file_type::name(self.file_type)
    }
}

/// 一条目录项所需的对齐后长度。
///
/// 对应内核宏 `EXT4_DIR_REC_LEN(name_len)`。
#[inline]
pub fn ext4_dir_rec_len(name_len: u8) -> u16 {
    let raw = EXT4_DIR_ENTRY_HEADER_LEN + name_len as usize;
    // 向上取到 4 字节边界
    ((raw + 3) & !3) as u16
}

/// 目录块迭代器
///
/// 遍历一个目录**数据块**里的所有项。ext4 的目录项不跨块，所以调用方
/// 每读一个块就新建一个迭代器。
///
/// 损坏防护（内核 `ext4_check_dir_entry()` 的等价物）：
/// - `rec_len` 为 0 或不是 4 的倍数 → 停止（否则原地死循环）
/// - `rec_len` 小于头 + name_len → 停止
/// - 越过块尾 → 停止
pub struct DirIter<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> DirIter<'a> {
    /// 在一个目录数据块上建迭代器
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// 当前偏移
    #[inline]
    pub fn offset(&self) -> usize {
        self.pos
    }

    /// 取一条项的名字字节（不含 NUL）
    pub fn name_bytes(&self, e: &Ext4DirEntry) -> &'a [u8] {
        let end = e.name_off + e.name_len as usize;
        if end <= self.buf.len() {
            &self.buf[e.name_off..end]
        } else {
            &[]
        }
    }
}

impl<'a> Iterator for DirIter<'a> {
    type Item = Ext4DirEntry;

    fn next(&mut self) -> Option<Ext4DirEntry> {
        // 头都放不下了
        if self.pos + EXT4_DIR_ENTRY_HEADER_LEN > self.buf.len() {
            return None;
        }
        let b = &self.buf[self.pos..];
        let inode = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let rec_len = u16::from_le_bytes([b[4], b[5]]);
        let name_len = b[6];
        let file_type = b[7];

        // 损坏检查
        if rec_len < EXT4_DIR_ENTRY_HEADER_LEN as u16
            || rec_len % 4 != 0
            || (rec_len as usize) < EXT4_DIR_ENTRY_HEADER_LEN + name_len as usize
        {
            return None;
        }
        // Check that the entry data (header + name) fits in buffer.
        // rec_len may extend beyond buffer for the last entry in a dir block
        // (e.g., 4096-byte block read as 1024-byte sub-blocks).
        let entry_end = self.pos + EXT4_DIR_ENTRY_HEADER_LEN + name_len as usize;
        if entry_end > self.buf.len() {
            return None;
        }

        let e = Ext4DirEntry {
            inode,
            rec_len,
            name_len,
            file_type,
            name_off: self.pos + EXT4_DIR_ENTRY_HEADER_LEN,
        };
        // Advance by rec_len, but don't go past the buffer
        let next = self.pos + rec_len as usize;
        self.pos = if next > self.buf.len() { self.buf.len() } else { next };
        Some(e)
    }
}

/// 在一个目录块里按名字查 inode 号。
///
/// 跳过空槽（`inode == 0`）。对应内核 `ext4_find_entry()` 的线性扫描分支
/// （没开 dir_index 时走的就是这条）。
pub fn find_entry(buf: &[u8], name: &[u8]) -> Option<u32> {
    if name.is_empty() || name.len() > EXT4_NAME_LEN {
        return None;
    }
    for e in DirIter::new(buf) {
        if e.is_empty() || e.name_len as usize != name.len() {
            continue;
        }
        let start = e.name_off;
        let end = start + e.name_len as usize;
        if end <= buf.len() && &buf[start..end] == name {
            return Some(e.inode);
        }
    }
    None
}

#[cfg(feature = "extra-drivers")]
/// 把一项填进用户给的 [`Dirent`]。对应 ext4 的 `ext4_readdir`，但沿用
/// 本树单项语义（一次 `getdents` 返回一项）。
///
/// `pos` 是目录内的字节偏移（`filp->f_pos`）。`Dirent.d_name` 只有 32 字节，
/// 名字超长截断。返回下一次的 `f_pos`，到末尾返回 0。
///
/// # Safety
/// 只能在进程上下文调用。`n` 必须是已 `iget` 的目录 inode。
pub unsafe fn fill_dirent(n: usize, pos: u64, out: &mut crate::fs::Dirent) -> i64 {
    use crate::fs::buffer::{self, BLOCK_SIZE, NIL, bh};
    use crate::fs::inode;
    use crate::fs::super_block::sb;
    use crate::klib::errno::{EBADF, ENOTDIR};
    use crate::fs::mode;

    unsafe {
        if !mode::is_dir(inode::inode(n).i_mode) {
            return -(ENOTDIR as i64);
        }
        let sb_nr = inode::inode(n).i_sb;
        if sb_nr == NIL {
            return -(EBADF as i64);
        }
        let dev = sb(sb_nr).s_dev;
        let size = inode::inode(n).i_size as u64;
        let info = crate::fs::ext4::ops::full::ext4_info(sb_nr);
        let fs_block = if info.fs_block_size > 0 { info.fs_block_size as u64 } else { BLOCK_SIZE as u64 };
        let scale = (fs_block / BLOCK_SIZE as u64).max(1);
        let mut off = pos;

        while off < size {
            let block_fs = (off / fs_block) as u32;
            // 读整个文件系统块，目录项不按 1024 字节对齐，必须整块解析。
            let mut buf = [0u8; 4096];
            let nbytes = (scale as usize) * BLOCK_SIZE;
            let mut valid = scale <= 4;
            if valid {
                for sub in 0..scale {
                    let phys = crate::fs::ext4::ops::full::bmap(n, (block_fs as u64 * scale + sub) as u32, false);
                    if phys == 0 { valid = false; break; }
                    let bn = match buffer::bread(dev, phys, BLOCK_SIZE) {
                        Some(b) => b, None => { valid = false; break; }
                    };
                    let d = bh(bn).data();
                    let s = (sub as usize) * BLOCK_SIZE;
                    buf[s..s + BLOCK_SIZE].copy_from_slice(&d[..BLOCK_SIZE]);
                    buffer::brelse(bn);
                }
            }
            if !valid { off = (block_fs as u64 + 1) * fs_block; continue; }

            let block_base = (block_fs as u64) * fs_block;
            let cur = (off - block_base) as usize;
            // 从块内偏移 cur 起找第一个有效（inode!=0）目录项。
            let mut chosen: Option<(Ext4DirEntry, usize)> = None;
            for e in DirIter::new(&buf[..nbytes]) {
                let e_off = e.name_off - EXT4_DIR_ENTRY_HEADER_LEN;
                if e_off < cur { continue; }
                if !e.is_empty() {
                    chosen = Some((e, e_off));
                    break;
                }
            }
            if let Some((e, e_off)) = chosen {
                let nlen = (e.name_len as usize).min(out.d_name.len());
                let nstart = e.name_off;
                out.d_ino = e.inode as u64;
                out.d_off = (block_base + e_off as u64 + e.rec_len as u64) as i64;
                out.d_reclen = core::mem::size_of::<crate::fs::Dirent>() as u16;
                out.d_type = e.file_type; // EXT4_FT_* 与 DT_* 同值
                out.d_name = [0; 32];
                if nstart + nlen <= nbytes {
                    out.d_name[..nlen].copy_from_slice(&buf[nstart..nstart + nlen]);
                }
                return (block_base + e_off as u64 + e.rec_len as u64) as i64;
            }
            // 本块没有更多有效项：跳到下一块
            off = (block_fs as u64 + 1) * fs_block;
        }
        0
    }
}
