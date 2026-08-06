//! ext4 块组描述符
//!
//! 对应内核 `struct ext4_group_desc`（`fs/ext4/ext4.h`）。ext2 的描述符是
//! 32 字节，ext4 开了 `INCOMPAT_64BIT` 之后是 64 字节（`s_desc_size`），
//! 后 32 字节放各字段的高 32 位。
//!
//! 描述符表紧跟在超级块所在块之后：块大小 1024 时超级块占 block 1，
//! 表从 block 2 开始；块大小 >= 2048 时超级块和引导区共享 block 0，
//! 表从 block 1 开始。这个规则由 `s_first_data_block` 表达。

/// ext2/ext3 的描述符大小
pub const EXT2_MIN_DESC_SIZE: usize = 32;
/// 开了 64bit 特性后的描述符大小
pub const EXT4_MIN_DESC_SIZE_64BIT: usize = 64;

/// 块组描述符（已把高低位合并成 64 位量）
#[derive(Debug, Clone, Copy, Default)]
pub struct Ext4GroupDesc {
    /// 块位图所在物理块号
    pub block_bitmap: u64,
    /// inode 位图所在物理块号
    pub inode_bitmap: u64,
    /// inode 表起始物理块号
    pub inode_table: u64,
    /// 空闲块数
    pub free_blocks_count: u32,
    /// 空闲 inode 数
    pub free_inodes_count: u32,
    /// 已用目录数
    pub used_dirs_count: u32,
    /// 标志（EXT4_BG_*）
    pub flags: u16,
    /// 本组未使用的 inode 数（GDT_CSUM 优化用）
    pub itable_unused: u32,
    /// 描述符校验和
    pub checksum: u16,
}

/// inode 表未初始化
pub const EXT4_BG_INODE_UNINIT: u16 = 0x0001;
/// 块位图未初始化
pub const EXT4_BG_BLOCK_UNINIT: u16 = 0x0002;
/// inode 表已清零
pub const EXT4_BG_INODE_ZEROED: u16 = 0x0004;

impl Ext4GroupDesc {
    /// 从磁盘字节解析。
    ///
    /// `desc_size` 是超级块里的 `s_desc_size`（0 视为 32）。只有
    /// `desc_size >= 64` 时才读高 32 位字段——ext2 的 32 字节描述符里
    /// 那些偏移是别的东西（padding / reserved），当高位读会得到垃圾。
    pub fn from_bytes(data: &[u8], desc_size: usize) -> Option<Self> {
        let size = if desc_size == 0 { EXT2_MIN_DESC_SIZE } else { desc_size };
        if data.len() < size || size < EXT2_MIN_DESC_SIZE {
            return None;
        }
        let rd32 = |o: usize| -> u32 {
            u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
        };
        let rd16 = |o: usize| -> u16 { u16::from_le_bytes([data[o], data[o + 1]]) };

        let mut g = Self {
            block_bitmap: rd32(0) as u64,
            inode_bitmap: rd32(4) as u64,
            inode_table: rd32(8) as u64,
            free_blocks_count: rd16(12) as u32,
            free_inodes_count: rd16(14) as u32,
            used_dirs_count: rd16(16) as u32,
            flags: rd16(18),
            itable_unused: rd16(28) as u32,
            checksum: rd16(30),
        };

        if size >= EXT4_MIN_DESC_SIZE_64BIT {
            g.block_bitmap |= (rd32(32) as u64) << 32;
            g.inode_bitmap |= (rd32(36) as u64) << 32;
            g.inode_table |= (rd32(40) as u64) << 32;
            g.free_blocks_count |= (rd16(44) as u32) << 16;
            g.free_inodes_count |= (rd16(46) as u32) << 16;
            g.used_dirs_count |= (rd16(48) as u32) << 16;
            g.itable_unused |= (rd16(50) as u32) << 16;
        }
        Some(g)
    }

    /// inode 表是否还没初始化
    #[inline]
    pub fn inode_table_uninit(&self) -> bool {
        self.flags & EXT4_BG_INODE_UNINIT != 0
    }

    /// 块位图是否还没初始化
    #[inline]
    pub fn block_bitmap_uninit(&self) -> bool {
        self.flags & EXT4_BG_BLOCK_UNINIT != 0
    }
}

/// 由 inode 号算出 (块组号, 组内下标)。
///
/// ext4 的 inode 号从 1 开始，对应内核 `ext4_get_inode_loc()` 的前半段。
/// `inodes_per_group` 为 0 或 `ino` 为 0 时返回 `None`。
pub fn ino_to_group(ino: u32, inodes_per_group: u32) -> Option<(u32, u32)> {
    if ino == 0 || inodes_per_group == 0 {
        return None;
    }
    let idx = ino - 1;
    Some((idx / inodes_per_group, idx % inodes_per_group))
}

/// 由 inode 号算出它在磁盘上的 (物理块号, 块内字节偏移)。
///
/// 对应内核 `ext4_get_inode_loc()`。`inode_size` 是超级块的
/// `s_inode_size`（128/256/512/1024），`block_size` 是文件系统块大小。
pub fn ino_to_disk(
    ino: u32,
    inodes_per_group: u32,
    inode_size: u32,
    block_size: u32,
    inode_table: u64,
) -> Option<(u64, u32)> {
    if inode_size == 0 || block_size == 0 {
        return None;
    }
    let (_, index) = ino_to_group(ino, inodes_per_group)?;
    let byte_off = index as u64 * inode_size as u64;
    Some((
        inode_table + byte_off / block_size as u64,
        (byte_off % block_size as u64) as u32,
    ))
}
