//! ext4 inode 结构
//! 
//! ext4 inode 扩展了 ext2 inode，增加了以下字段：
//! - nanosecond 时间戳精度
//! - i_crtime: 创建时间
//! - i_version: inode 版本
//! - 大文件支持

use super::inode_flags::*;

/// ext4 inode 结构
/// 
/// ext4 inode 大小默认是 256 字节 (可配置为 128, 256, 512, 1024)
/// 与 ext2 inode 完全兼容，前 128 字节相同
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Ext4Inode {
    // ext2 兼容字段 (前 128 字节)
    /// 文件模式
    pub i_mode: u16,
    /// 保留 uid
    pub i_uid: u16,
    /// 文件大小低位
    pub i_size_lo: u32,
    /// 最后访问时间 (atime)
    pub i_atime: u32,
    /// inode 变化时间 (ctime)
    pub i_ctime: u32,
    /// 修改时间 (mtime)
    pub i_mtime: u32,
    /// 删除时间 (dtime)
    pub i_dtime: u32,
    /// gid
    pub i_gid: u16,
    /// 硬链接计数
    pub i_links_count: u16,
    /// 数据块计数
    pub i_blocks_lo: u32,
    /// 标志
    pub i_flags: u32,
    /// 操作系统特定值 1
    pub i_osd1: u32,
    /// 直接块指针 (12 个)
    pub i_block: [u32; 12],
    /// 间接块号
    pub i_block_indirect: u32,
    /// 双间接块号
    pub i_block_double_indirect: u32,
    /// 三间接块号
    pub i_block_triple_indirect: u32,
    /// 世代数
    pub i_generation: u32,
    /// 扩展属性块（低 32 位）
    pub i_file_acl: u32,
    /// 文件大小高 32 位（老名字 i_dir_acl）
    pub i_size_high: u32,
    /// 碎片块号（ext4 已废弃，恒为 0）
    pub i_fragment_addr: u32,
    /// 操作系统特定值 2。Linux 下是
    /// `l_i_blocks_hi(2) l_i_file_acl_high(2) l_i_uid_high(2)
    ///  l_i_gid_high(2) l_i_checksum_lo(2) l_i_reserved(2)`
    pub i_osd2: [u8; 12],

    // ---- ext4 额外字段（偏移 128 起，仅当 i_extra_isize 覆盖到才有效）----
    /// 本 inode 用掉的额外字节数。它自己在偏移 128，是判断后面字段
    /// 是否存在的依据（内核 `EXT4_FITS_IN_INODE` 宏）。
    pub i_extra_isize: u16,
    /// inode 校验和高 16 位
    pub i_checksum_hi: u16,
    /// ctime 的纳秒 + 纪元高位
    pub i_ctime_extra: u32,
    /// mtime 的纳秒 + 纪元高位
    pub i_mtime_extra: u32,
    /// atime 的纳秒 + 纪元高位
    pub i_atime_extra: u32,
    /// 创建时间（秒）
    pub i_crtime: u32,
    /// 创建时间的纳秒 + 纪元高位
    pub i_crtime_extra: u32,
    /// inode 版本高 32 位
    pub i_version_hi: u32,
    /// project id
    pub i_projid: u32,
}

impl Ext4Inode {
    /// ext4 默认的磁盘 inode 大小
    pub const SIZE: usize = 256;
    /// ext2 老格式的 inode 大小，也是解析所需的最小字节数
    pub const MIN_SIZE: usize = 128;
    
    /// 从磁盘字节解析一个 inode。
    ///
    /// `data` 至少要有 [`Self::MIN_SIZE`]（128）字节；不足 256 字节时
    /// ext4 额外字段全部按 0 处理（那是 ext2 的 128 字节 inode）。
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_SIZE {
            return None;
        }

        // 越界返 0 的读取器：128 字节 inode 走这条路，读到 128 之后全是 0。
        fn rd16(d: &[u8], o: usize) -> u16 {
            if o + 2 <= d.len() { u16::from_le_bytes([d[o], d[o + 1]]) } else { 0 }
        }
        fn rd32(d: &[u8], o: usize) -> u32 {
            if o + 4 <= d.len() {
                u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
            } else {
                0
            }
        }

        let mut inode = Self {
            i_mode: u16::from_le_bytes([data[0], data[1]]),
            i_uid: u16::from_le_bytes([data[2], data[3]]),
            i_size_lo: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            i_atime: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            i_ctime: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            i_mtime: u32::from_le_bytes([data[16], data[17], data[18], data[19]]),
            i_dtime: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
            i_gid: u16::from_le_bytes([data[24], data[25]]),
            i_links_count: u16::from_le_bytes([data[26], data[27]]),
            i_blocks_lo: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            i_flags: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            i_osd1: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
            i_block: {
                let mut blocks = [0u32; 12];
                for i in 0..12 {
                    let offset = 40 + i * 4;
                    blocks[i] = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
                }
                blocks
            },
            i_block_indirect: u32::from_le_bytes([data[88], data[89], data[90], data[91]]),
            i_block_double_indirect: u32::from_le_bytes([data[92], data[93], data[94], data[95]]),
            i_block_triple_indirect: u32::from_le_bytes([data[96], data[97], data[98], data[99]]),
            i_generation: u32::from_le_bytes([data[100], data[101], data[102], data[103]]),
            i_file_acl: u32::from_le_bytes([data[104], data[105], data[106], data[107]]),
            i_size_high: u32::from_le_bytes([data[108], data[109], data[110], data[111]]),
            i_fragment_addr: u32::from_le_bytes([data[112], data[113], data[114], data[115]]),
            i_osd2: {
                let mut osd2 = [0u8; 12];
                osd2.copy_from_slice(&data[116..128]);
                osd2
            },
            // 128 字节 inode（ext2 老格式）没有下面这些字段。逐个字段
            // 单独判长度是原来的写法，但那样 i_extra_isize 说了不算——
            // 真正的判据是 128 + i_extra_isize 是否覆盖到该字段（内核
            // EXT4_FITS_IN_INODE）。这里先按缓冲长度取，再用 extra_isize
            // 把没覆盖到的清掉。
            i_extra_isize: rd16(data, 128),
            i_checksum_hi: rd16(data, 130),
            i_ctime_extra: rd32(data, 132),
            i_mtime_extra: rd32(data, 136),
            i_atime_extra: rd32(data, 140),
            i_crtime: rd32(data, 144),
            i_crtime_extra: rd32(data, 148),
            i_version_hi: rd32(data, 152),
            i_projid: rd32(data, 156),
        };

        // i_extra_isize 之后没覆盖到的额外字段按「不存在」处理，避免把
        // 别人的 xattr 数据当时间戳用。
        let covered = 128usize + inode.i_extra_isize as usize;
        let fits = |end: usize| end <= covered;
        if !fits(132) { inode.i_checksum_hi = 0 }
        if !fits(136) { inode.i_ctime_extra = 0 }
        if !fits(140) { inode.i_mtime_extra = 0 }
        if !fits(144) { inode.i_atime_extra = 0 }
        if !fits(148) { inode.i_crtime = 0 }
        if !fits(152) { inode.i_crtime_extra = 0 }
        if !fits(156) { inode.i_version_hi = 0 }
        if !fits(160) { inode.i_projid = 0 }
        Some(inode)
    }
    
    /// 从 `i_osd2` 里取一个小端 u16（Linux 的 `osd2.linux2` 各字段）
    #[inline]
    fn osd2_u16(&self, off: usize) -> u16 {
        let b = self.i_osd2;
        u16::from_le_bytes([b[off], b[off + 1]])
    }

    /// 文件大小（64 位）。
    ///
    /// 只有常规文件才用 `i_size_high` 当高 32 位——目录里那个字段是老的
    /// `i_dir_acl`。内核 `ext4_isize()` 同样只对 S_ISREG 合并高位。
    pub fn i_size(&self) -> u64 {
        if self.is_reg() {
            ((self.i_size_high as u64) << 32) | (self.i_size_lo as u64)
        } else {
            self.i_size_lo as u64
        }
    }

    /// 512 字节扇区数（48 位）。高 16 位在 `i_osd2` 的 `l_i_blocks_hi`。
    pub fn i_blocks(&self) -> u64 {
        ((self.osd2_u16(0) as u64) << 32) | (self.i_blocks_lo as u64)
    }

    /// 硬链接数。ext4 的 `i_links_count` 就是 16 位，没有高位扩展；
    /// 目录链接数超过 65000 时内核把它写成 1（`EXT4_LINK_MAX` 语义）。
    pub fn links_count(&self) -> u32 {
        self.i_links_count as u32
    }

    /// UID（32 位）。高 16 位在 `i_osd2` 的 `l_i_uid_high`（偏移 4）。
    pub fn uid(&self) -> u32 {
        ((self.osd2_u16(4) as u32) << 16) | (self.i_uid as u32)
    }

    /// GID（32 位）。高 16 位在 `i_osd2` 的 `l_i_gid_high`（偏移 6）。
    pub fn gid(&self) -> u32 {
        ((self.osd2_u16(6) as u32) << 16) | (self.i_gid as u32)
    }

    /// `i_block` 那 60 字节的原始内容。
    ///
    /// 走 extent 的 inode 把 extent 树根（12 字节头 + 最多 4 条）塞在这里，
    /// 所以不能只当 15 个 u32 块号看。这里把结构体里拆开存的三个间接块号
    /// 拼回去，还原成磁盘上连续的 60 字节。
    pub fn i_block_raw(&self) -> [u8; 60] {
        let mut out = [0u8; 60];
        for i in 0..12 {
            out[i * 4..i * 4 + 4].copy_from_slice(&self.i_block[i].to_le_bytes());
        }
        out[48..52].copy_from_slice(&self.i_block_indirect.to_le_bytes());
        out[52..56].copy_from_slice(&self.i_block_double_indirect.to_le_bytes());
        out[56..60].copy_from_slice(&self.i_block_triple_indirect.to_le_bytes());
        out
    }
    
    /// 获取访问时间 (秒)
    pub fn atime(&self) -> u32 {
        self.i_atime
    }
    
    /// 获取修改时间 (秒)
    pub fn mtime(&self) -> u32 {
        self.i_mtime
    }
    
    /// 获取变化时间 (秒)
    pub fn ctime(&self) -> u32 {
        self.i_ctime
    }
    
    // 类型判断必须先用 S_IFMT 掩掉低位再比相等。原来写的是
    // `mode & 0x4000 != 0` 之类：0xA000（符号链接）会同时被 is_dir
    // 判成真（0xA000 & 0x4000 == 0）——实际上 0xA000 & 0x8000 != 0，
    // 于是符号链接被 is_reg 认成常规文件，而 i_size 又只对常规文件
    // 合并高 32 位，链接目标长度会被当成 64 位大小读。

    /// 是否是目录
    pub fn is_dir(&self) -> bool {
        self.i_mode & mode::S_IFMT == mode::S_IFDIR
    }

    /// 是否是常规文件
    pub fn is_reg(&self) -> bool {
        self.i_mode & mode::S_IFMT == mode::S_IFREG
    }

    /// 是否是符号链接
    pub fn is_symlink(&self) -> bool {
        self.i_mode & mode::S_IFMT == mode::S_IFLNK
    }

    /// 是否是快速符号链接（目标直接存在 `i_block` 的 60 字节里）。
    ///
    /// 判据是内核的 `ext4_inode_is_fast_symlink()`：符号链接且没有分配
    /// 数据块。LFS 的 `/lib`、`/bin` 之类几乎全是快速符号链接。
    pub fn is_fast_symlink(&self) -> bool {
        self.is_symlink() && self.i_blocks() == 0 && self.i_size_lo as usize <= 60
    }
    
    /// 检查是否使用 extent
    pub fn uses_extent(&self) -> bool {
        (self.i_flags & EXT4_EXTENTS_FL) != 0
    }
    
    /// 获取直接块数量
    pub fn direct_blocks(&self) -> usize {
        12
    }
    
    /// 获取间接块数量
    pub fn indirect_blocks(&self) -> usize {
        crate::mm::page::PAGE_SIZE as usize / 4
    }
    
    /// 获取双间接块数量
    pub fn double_indirect_blocks(&self) -> usize {
        let indirect = self.indirect_blocks();
        indirect * indirect
    }
    
    /// 获取三间接块数量
    pub fn triple_indirect_blocks(&self) -> usize {
        let indirect = self.indirect_blocks();
        indirect * indirect * indirect
    }
}

/// 文件权限
pub mod mode {
    pub const TYPE_MASK: u16 = 0xF000;
    pub const S_IFMT: u16 = 0xF000;
    pub const S_IFSOCK: u16 = 0xC000;
    pub const S_IFLNK: u16 = 0xA000;
    pub const S_IFREG: u16 = 0x8000;
    pub const S_IFBLK: u16 = 0x6000;
    pub const S_IFDIR: u16 = 0x4000;
    pub const S_IFCHR: u16 = 0x2000;
    pub const S_IFIFO: u16 = 0x1000;
    
    /// 从 mode 提取文件类型
    pub fn file_type(mode: u16) -> u16 {
        mode & TYPE_MASK
    }
    
    /// 检查是否是常规文件
    pub fn is_reg(mode: u16) -> bool {
        file_type(mode) == S_IFREG
    }
    
    /// 检查是否是目录
    pub fn is_dir(mode: u16) -> bool {
        file_type(mode) == S_IFDIR
    }
    
    /// 检查是否是字符设备
    pub fn is_chr(mode: u16) -> bool {
        file_type(mode) == S_IFCHR
    }
    
    /// 检查是否是块设备
    pub fn is_blk(mode: u16) -> bool {
        file_type(mode) == S_IFBLK
    }
    
    /// 检查是否是套接字
    pub fn is_sock(mode: u16) -> bool {
        file_type(mode) == S_IFSOCK
    }
    
    /// 检查是否是符号链接
    pub fn is_lnk(mode: u16) -> bool {
        file_type(mode) == S_IFLNK
    }
    
    /// 检查是否是 FIFO
    pub fn is_fifo(mode: u16) -> bool {
        file_type(mode) == S_IFIFO
    }
}
