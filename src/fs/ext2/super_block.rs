//! ext2 超级块结构定义
//! 
//! 参考 linux-1.0.9/fs/ext2/super.c 和 include/linux/ext2_fs.h
//! 
//! 这些结构定义与磁盘格式对应，需要与缓冲区缓存集成才能使用。

// =============================================================================
// ext2 Constants
// =============================================================================

/// ext2 魔数
pub const EXT2_SUPER_MAGIC: u16 = 0xEF53;

/// ext2 状态标志
pub mod state {
    pub const CLEAN: u16 = 0x0001;
    pub const DIRTY: u16 = 0x0002;
}

/// ext2 错误处理模式
pub mod errors {
    pub const CONTINUE: u16 = 1;
    pub const REMOUNT_RO: u16 = 2;
    pub const PANIC: u16 = 3;
}

/// ext3 日志操作码
pub mod journal_ops {
    pub const DELETE: u16 = 0x0001;
    pub const RECOVER: u16 = 0x0002;
    pub const COMMIT: u16 = 0x0003;
}

/// ext2 修订级别
pub mod revision {
    pub const GOOD_OLD: u32 = 0;
    pub const DYNAMIC: u32 = 1;
}

/// ext2 特性兼容标志
pub mod feature_compat {
    pub const DIR_PREALLOC: u32 = 0x0001;
    pub const IMAGIC_INODES: u32 = 0x0002;
    pub const HAS_JOURNAL: u32 = 0x0004;
    pub const EXT_ATTR: u32 = 0x0008;
    pub const RESIZE_INODE: u32 = 0x0010;
    pub const DIR_INDEX: u32 = 0x0020;
}

/// ext2 特性只读兼容标志
pub mod feature_ro_compat {
    pub const SPARSE_SUPER: u32 = 0x0001;
    pub const LARGE_FILE: u32 = 0x0002;
    pub const BTREE_DIR: u32 = 0x0004;
}

/// ext2 特性不兼容标志
pub mod feature_incompat {
    pub const COMPRESSION: u32 = 0x0001;
    pub const FILETYPE: u32 = 0x0002;
    pub const RECOVER: u32 = 0x0004;
    pub const JOURNAL_DEV: u32 = 0x0008;
    pub const META_BG: u32 = 0x0010;
}

// =============================================================================
// ext2 On-Disk Superblock Structure
// =============================================================================

/// ext2 超级块结构（1024 字节）
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Ext2SuperBlock {
    pub s_inodes_count: u32,
    pub s_blocks_count: u32,
    pub s_r_blocks_count: u32,
    pub s_free_blocks_count: u32,
    pub s_free_inodes_count: u32,
    pub s_first_data_block: u32,
    pub s_log_block_size: u32,
    pub s_log_frag_size: u32,
    pub s_blocks_per_group: u32,
    pub s_fragments_per_group: u32,
    pub s_inodes_per_group: u32,
    pub s_mtime: u32,
    pub s_wtime: u32,
    pub s_mnt_count: u16,
    pub s_max_mnt_count: u16,
    pub s_magic: u16,
    pub s_state: u16,
    pub s_errors: u16,
    pub s_minor_rev_level: u16,
    pub s_lastcheck: u32,
    pub s_checkinterval: u32,
    pub s_creator_os: u32,
    pub s_rev_level: u32,
    pub s_def_resuid: u16,
    pub s_def_resgid: u16,
    pub s_first_ino: u32,
    pub s_inode_size: u16,
    pub s_block_group_nr: u16,
    pub s_feature_compat: u32,
    pub s_feature_incompat: u32,
    pub s_feature_ro_compat: u32,
    pub s_uuid: [u8; 16],
    pub s_volume_name: [u8; 16],
    pub s_last_mounted: [u8; 64],
    pub s_algo_bitmap: u32,
    pub s_prealloc_blocks: u8,
    pub s_prealloc_dir_blocks: u8,
    pub s_padding1: u16,
    pub s_journal_uuid: [u8; 16],
    pub s_journal_inum: u32,
    pub s_journal_dev: u32,
    pub s_last_orphan: u32,
    pub s_hash_seed: [u32; 4],
    pub s_def_hash_version: u8,
    pub s_reserved_char_pad: u8,
    pub s_reserved_word_pad: u16,
    pub s_default_mount_opts: u32,
    pub s_first_meta_bg: u32,
    pub s_reserved: [u32; 190],
}

impl Ext2SuperBlock {
    /// 从字节数组读取超级块
    pub unsafe fn from_bytes(data: &[u8; 1024]) -> Self {
        Self {
            s_inodes_count: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            s_blocks_count: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            s_r_blocks_count: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            s_free_blocks_count: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            s_free_inodes_count: u32::from_le_bytes([data[16], data[17], data[18], data[19]]),
            s_first_data_block: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
            s_log_block_size: u32::from_le_bytes([data[24], data[25], data[26], data[27]]),
            s_log_frag_size: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            s_blocks_per_group: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            s_fragments_per_group: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
            s_inodes_per_group: u32::from_le_bytes([data[40], data[41], data[42], data[43]]),
            s_mtime: u32::from_le_bytes([data[44], data[45], data[46], data[47]]),
            s_wtime: u32::from_le_bytes([data[48], data[49], data[50], data[51]]),
            s_mnt_count: u16::from_le_bytes([data[52], data[53]]),
            s_max_mnt_count: u16::from_le_bytes([data[54], data[55]]),
            s_magic: u16::from_le_bytes([data[56], data[57]]),
            s_state: u16::from_le_bytes([data[58], data[59]]),
            s_errors: u16::from_le_bytes([data[60], data[61]]),
            s_minor_rev_level: u16::from_le_bytes([data[62], data[63]]),
            s_lastcheck: u32::from_le_bytes([data[64], data[65], data[66], data[67]]),
            s_checkinterval: u32::from_le_bytes([data[68], data[69], data[70], data[71]]),
            s_creator_os: u32::from_le_bytes([data[72], data[73], data[74], data[75]]),
            s_rev_level: u32::from_le_bytes([data[76], data[77], data[78], data[79]]),
            s_def_resuid: u16::from_le_bytes([data[80], data[81]]),
            s_def_resgid: u16::from_le_bytes([data[82], data[83]]),
            s_first_ino: u32::from_le_bytes([data[84], data[85], data[86], data[87]]),
            s_inode_size: u16::from_le_bytes([data[88], data[89]]),
            s_block_group_nr: u16::from_le_bytes([data[90], data[91]]),
            s_feature_compat: u32::from_le_bytes([data[92], data[93], data[94], data[95]]),
            s_feature_incompat: u32::from_le_bytes([data[96], data[97], data[98], data[99]]),
            s_feature_ro_compat: u32::from_le_bytes([data[100], data[101], data[102], data[103]]),
            s_uuid: {
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(&data[104..120]);
                uuid
            },
            s_volume_name: {
                let mut name = [0u8; 16];
                name.copy_from_slice(&data[120..136]);
                name
            },
            s_last_mounted: {
                let mut mnt = [0u8; 64];
                mnt.copy_from_slice(&data[136..200]);
                mnt
            },
            s_algo_bitmap: u32::from_le_bytes([data[200], data[201], data[202], data[203]]),
            s_prealloc_blocks: data[204],
            s_prealloc_dir_blocks: data[205],
            s_padding1: u16::from_le_bytes([data[206], data[207]]),
            s_journal_uuid: {
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(&data[208..224]);
                uuid
            },
            s_journal_inum: u32::from_le_bytes([data[224], data[225], data[226], data[227]]),
            s_journal_dev: u32::from_le_bytes([data[228], data[229], data[230], data[231]]),
            s_last_orphan: u32::from_le_bytes([data[232], data[233], data[234], data[235]]),
            s_hash_seed: {
                let mut seed = [0u32; 4];
                for i in 0..4 {
                    let offset = 236 + i * 4;
                    seed[i] = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
                }
                seed
            },
            s_def_hash_version: data[252],
            s_reserved_char_pad: data[253],
            s_reserved_word_pad: u16::from_le_bytes([data[254], data[255]]),
            s_default_mount_opts: u32::from_le_bytes([data[256], data[257], data[258], data[259]]),
            s_first_meta_bg: u32::from_le_bytes([data[260], data[261], data[262], data[263]]),
            s_reserved: {
                let mut r = [0u32; 190];
                for i in 0..190 {
                    let offset = 264 + i * 4;
                    r[i] = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
                }
                r
            },
        }
    }

    /// 验证超级块魔数
    pub fn is_valid(&self) -> bool {
        self.s_magic == EXT2_SUPER_MAGIC
    }

    /// 获取块大小（字节数）
    pub fn block_size(&self) -> usize {
        1024 << self.s_log_block_size
    }

    /// 获取块组数量
    pub fn group_count(&self) -> u32 {
        let blocks = self.s_blocks_count;
        let blocks_per_group = self.s_blocks_per_group;
        if blocks % blocks_per_group == 0 {
            blocks / blocks_per_group
        } else {
            blocks / blocks_per_group + 1
        }
    }

    /// 获取 inode 大小
    pub fn inode_size(&self) -> u16 {
        if self.s_rev_level >= revision::DYNAMIC {
            if self.s_inode_size == 0 {
                128
            } else {
                self.s_inode_size
            }
        } else {
            128
        }
    }

    /// 检查是否有 ext3 日志特性
    pub fn has_journal(&self) -> bool {
        (self.s_feature_compat & feature_compat::HAS_JOURNAL) != 0
    }
}

// =============================================================================
// Block Group Descriptor
// =============================================================================

/// 块组描述符结构
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Ext2GroupDesc {
    pub bg_block_bitmap: u32,
    pub bg_inode_bitmap: u32,
    pub bg_inode_table: u32,
    pub bg_free_blocks_count: u16,
    pub bg_free_inodes_count: u16,
    pub bg_used_dirs_count: u16,
    pub bg_pad: u16,
    pub bg_reserved: [u32; 3],
}

impl Ext2GroupDesc {
    /// 从字节数组读取块组描述符
    pub unsafe fn from_bytes(data: &[u8]) -> Self {
        debug_assert!(data.len() >= 32);
        Self {
            bg_block_bitmap: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            bg_inode_bitmap: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            bg_inode_table: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            bg_free_blocks_count: u16::from_le_bytes([data[12], data[13]]),
            bg_free_inodes_count: u16::from_le_bytes([data[14], data[15]]),
            bg_used_dirs_count: u16::from_le_bytes([data[16], data[17]]),
            bg_pad: u16::from_le_bytes([data[18], data[19]]),
            bg_reserved: {
                let mut r = [0u32; 3];
                for i in 0..3 {
                    let offset = 20 + i * 4;
                    r[i] = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
                }
                r
            },
        }
    }
}
