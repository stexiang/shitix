//! ext2 inode 结构定义
//! 
//! 参考 linux-1.0.9/fs/ext2/inode.c 和 include/linux/ext2_fs_i.h

// =============================================================================
// ext2 Inode Constants
// =============================================================================

/// inode 模式类型
pub mod mode {
    pub const FIFO: u16 = 0x1000;
    pub const CHARACTER_DEVICE: u16 = 0x2000;
    pub const DIRECTORY: u16 = 0x4000;
    pub const BLOCK_DEVICE: u16 = 0x6000;
    pub const REGULAR: u16 = 0x8000;
    pub const SYMBOLIC_LINK: u16 = 0xA000;
    pub const SOCKET: u16 = 0xC000;

    pub const OWNER_READ: u16 = 0x0100;
    pub const OWNER_WRITE: u16 = 0x0080;
    pub const OWNER_EXEC: u16 = 0x0040;
    pub const GROUP_READ: u16 = 0x0020;
    pub const GROUP_WRITE: u16 = 0x0010;
    pub const GROUP_EXEC: u16 = 0x0008;
    pub const OTHER_READ: u16 = 0x0004;
    pub const OTHER_WRITE: u16 = 0x0002;
    pub const OTHER_EXEC: u16 = 0x0001;

    pub fn file_type(mode: u16) -> u16 {
        mode & 0xF000
    }

    pub fn is_dir(mode: u16) -> bool {
        file_type(mode) == DIRECTORY
    }

    pub fn is_reg(mode: u16) -> bool {
        file_type(mode) == REGULAR
    }

    pub fn is_lnk(mode: u16) -> bool {
        file_type(mode) == SYMBOLIC_LINK
    }
}

/// 直接块指针数量
pub const EXT2_NDIR_BLOCKS: usize = 12;

/// 每块块指针数
pub const EXT2_ADDR_PER_BLOCK: usize = 256;

// =============================================================================
// ext2 Inode Structure
// =============================================================================

/// ext2 inode 结构（128/256 字节）
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Ext2Inode {
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size: u32,
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks: u32,
    pub i_flags: u32,
    pub i_osd1: u32,
    pub i_block: [u32; EXT2_NDIR_BLOCKS],
    pub i_block_1ind: u32,
    pub i_block_2ind: u32,
    pub i_block_3ind: u32,
    pub i_generation: u32,
    pub i_file_acl: u32,
    pub i_dir_acl: u32,
    pub i_fragment_address: u32,
    pub i_osd2: [u8; 12],
}

impl Ext2Inode {
    /// 从字节数组读取 inode
    pub unsafe fn from_bytes(data: &[u8], inode_size: u16) -> Self {
        let size = inode_size as usize;
        debug_assert!(data.len() >= size);
        
        Self {
            i_mode: u16::from_le_bytes([data[0], data[1]]),
            i_uid: u16::from_le_bytes([data[2], data[3]]),
            i_size: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            i_atime: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            i_ctime: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            i_mtime: u32::from_le_bytes([data[16], data[17], data[18], data[19]]),
            i_dtime: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
            i_gid: u16::from_le_bytes([data[24], data[25]]),
            i_links_count: u16::from_le_bytes([data[26], data[27]]),
            i_blocks: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            i_flags: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            i_osd1: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
            i_block: {
                let mut blocks = [0u32; EXT2_NDIR_BLOCKS];
                for i in 0..EXT2_NDIR_BLOCKS {
                    let offset = 40 + i * 4;
                    blocks[i] = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
                }
                blocks
            },
            i_block_1ind: {
                let offset = 40 + EXT2_NDIR_BLOCKS * 4;
                u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]])
            },
            i_block_2ind: {
                let offset = 40 + (EXT2_NDIR_BLOCKS + 1) * 4;
                u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]])
            },
            i_block_3ind: {
                let offset = 40 + (EXT2_NDIR_BLOCKS + 2) * 4;
                u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]])
            },
            i_generation: u32::from_le_bytes([data[96], data[97], data[98], data[99]]),
            i_file_acl: u32::from_le_bytes([data[100], data[101], data[102], data[103]]),
            i_dir_acl: u32::from_le_bytes([data[104], data[105], data[106], data[107]]),
            i_fragment_address: u32::from_le_bytes([data[108], data[109], data[110], data[111]]),
            i_osd2: {
                let mut osd2 = [0u8; 12];
                osd2.copy_from_slice(&data[112..124]);
                osd2
            },
        }
    }

    /// 检查是否为目录
    pub fn is_dir(&self) -> bool {
        mode::is_dir(self.i_mode)
    }

    /// 检查是否为常规文件
    pub fn is_reg(&self) -> bool {
        mode::is_reg(self.i_mode)
    }
}

// =============================================================================
// ext2 Directory Entry
// =============================================================================

/// ext2 目录项结构（变长）
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Ext2DirEntry {
    pub inode: u32,
    pub rec_len: u16,
    pub name_len: u16,
    // name follows here
}

impl Ext2DirEntry {
    /// 目录项最小长度
    pub const MIN_SIZE: usize = 8;
    
    /// 计算目录项占用的空间
    pub fn rec_len(name_len: u16) -> u16 {
        let mut rec_len = 8 + name_len;
        rec_len = (rec_len + 3) & !3u16; // 对齐到 4 字节
        rec_len
    }
}

// =============================================================================
// Inode Cache Info
// =============================================================================

/// ext2 inode 内存中的额外信息
#[derive(Debug)]
pub struct Ext2InodeInfo {
    pub raw_inode: Ext2Inode,
    pub i_ino: u32,
    pub dirty: bool,
}

impl Ext2InodeInfo {
    pub fn new(ino: u32, inode: Ext2Inode) -> Self {
        Self {
            raw_inode: inode,
            i_ino: ino,
            dirty: false,
        }
    }
}
