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
    /// 文件 ACL
    pub i_file_acl: u32,
    /// 目录 ACL
    pub i_size_high: u32,
    /// 碎片块号
    pub i_fragment_addr: u32,
    /// 操作系统特定值 2
    pub i_osd2: [u8; 12],
    
    // ext4 特定字段 (偏移 128)
    /// 创建时间 (ctime nanoseconds)
    pub i_crtime_extra: u32,
    /// 修改时间 (mtime nanoseconds)
    pub i_mtime_extra: u32,
    /// 访问时间 (atime nanoseconds)
    pub i_atime_extra: u32,
    /// 创建时间
    pub i_crtime: u32,
    /// 版本号
    pub i_version_hi: u32,
    /// 扩展属性大小
    pub i_extra_isize: u16,
    /// 保留
    pub i_pad1: u16,
    /// 保留用于 i_links_count
    pub i_links_count_hi: u16,
    /// 保留用于 i_uid
    pub i_uid_hi: u16,
    /// 保留用于 i_gid
    pub i_gid_hi: u16,
    /// 校验和
    pub i_checksum_lo: u16,
    /// 保留
    pub i_reserved: u16,
}

impl Ext4Inode {
    /// inode 大小 (ext4 默认 256)
    pub const SIZE: usize = 256;
    
    /// 从字节创建
    pub unsafe fn from_bytes(data: &[u8]) -> Self {
        debug_assert!(data.len() >= Self::SIZE);
        
        Self {
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
            i_crtime_extra: if data.len() >= 132 { u32::from_le_bytes([data[128], data[129], data[130], data[131]]) } else { 0 },
            i_mtime_extra: if data.len() >= 136 { u32::from_le_bytes([data[132], data[133], data[134], data[135]]) } else { 0 },
            i_atime_extra: if data.len() >= 140 { u32::from_le_bytes([data[136], data[137], data[138], data[139]]) } else { 0 },
            i_crtime: if data.len() >= 144 { u32::from_le_bytes([data[140], data[141], data[142], data[143]]) } else { 0 },
            i_version_hi: if data.len() >= 148 { u32::from_le_bytes([data[144], data[145], data[146], data[147]]) } else { 0 },
            i_extra_isize: if data.len() >= 150 { u16::from_le_bytes([data[148], data[149]]) } else { 0 },
            i_pad1: if data.len() >= 152 { u16::from_le_bytes([data[150], data[151]]) } else { 0 },
            i_links_count_hi: if data.len() >= 154 { u16::from_le_bytes([data[152], data[153]]) } else { 0 },
            i_uid_hi: if data.len() >= 156 { u16::from_le_bytes([data[154], data[155]]) } else { 0 },
            i_gid_hi: if data.len() >= 158 { u16::from_le_bytes([data[156], data[157]]) } else { 0 },
            i_checksum_lo: if data.len() >= 160 { u16::from_le_bytes([data[158], data[159]]) } else { 0 },
            i_reserved: if data.len() >= 162 { u16::from_le_bytes([data[160], data[161]]) } else { 0 },
        }
    }
    
    /// 获取文件大小 (64 位)
    pub fn i_size(&self) -> u64 {
        (self.i_size_high as u64) << 32 | (self.i_size_lo as u64)
    }
    
    /// 获取块数 (64 位支持)
    pub fn i_blocks(&self) -> u64 {
        self.i_blocks_lo as u64
    }
    
    /// 获取链接计数 (支持 > 65535)
    pub fn links_count(&self) -> u32 {
        ((self.i_links_count_hi as u32) << 16) | (self.i_links_count as u32)
    }
    
    /// 获取 UID (支持 > 65535)
    pub fn uid(&self) -> u32 {
        ((self.i_uid_hi as u32) << 16) | (self.i_uid as u32)
    }
    
    /// 获取 GID (支持 > 65535)
    pub fn gid(&self) -> u32 {
        ((self.i_gid_hi as u32) << 16) | (self.i_gid as u32)
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
    
    /// 检查是否是目录
    pub fn is_dir(&self) -> bool {
        (self.i_mode & 0x4000) != 0
    }
    
    /// 检查是否是常规文件
    pub fn is_reg(&self) -> bool {
        (self.i_mode & 0x8000) != 0
    }
    
    /// 检查是否是符号链接
    pub fn is_symlink(&self) -> bool {
        (self.i_mode & 0xA000) == 0xA000
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
