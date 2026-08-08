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
