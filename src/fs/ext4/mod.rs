//! ext4 文件系统实现
//! 
//! ext4 是 ext2/ext3 的超集，提供了以下主要特性：
//! - 大文件系统支持 (最大 1EB)
//! - Extent 树替代块位图 (更高效的存储)
//! - 延迟分配 (extent 分配优化)
//! - 日志校验和 (ext3 journal checksumming)
//! - 无日志模式 (no journal mode)
//! - 在线碎片整理
//! - 持久性预分配
//! - 默认 inode 32KB
//! 
//! ## ext4 特性
//! 
//! ### 只读兼容特性 (ro_compat)
//! - Sparse superblock v2
//! - Large file support (> 2TB files)
//! - Btree directory indexing
//! - Huge file support (> 16TB files)
//! - Flexible block groups
//! - Extended attribute inline storage
//! 
//! ### 不兼容特性 (incompat)
//! - Compression
//! - Directory entries with file type
//! - Recover journal
//! - Journal device
//! - Meta block groups
//! - File extent tree
//! - Flexible block group size
//! - Extended attribute inline storage
//! 
//! ## 磁盘格式
//! 
//! ext4 与 ext2/ext3 共享相同的磁盘布局，超级块位于偏移 1024 字节处。
//! ext4 主要在以下方面扩展了 ext2/ext3：
//! 1. 超级块中的新字段
//! 2. extent 数据结构
//! 3. inode 中的额外时间戳
//! 4. 改进的块组描述符

pub mod super_block;
pub mod inode;
pub mod extent;
pub mod feature;
pub mod group_desc;
pub mod dir;
pub mod selftest;
pub mod bitmap;
pub mod ops;
pub mod mkfs;
#[cfg(feature = "extra-drivers")]
pub mod namei;

// Re-exports
pub use super_block::{Ext4SuperBlock, Ext4FeatureFlags};
pub use inode::Ext4Inode;
pub use extent::{ExtentHeader, ExtentIdx, Extent, ExtentNode};
pub use feature::Ext4Features;
pub use group_desc::Ext4GroupDesc;
pub use dir::{Ext4DirEntry, DirIter};
pub use ops::read_super;

/// ext4 文件类型
pub mod file_type {
    pub const EXT4_FT_UNKNOWN: u8 = 0;
    pub const EXT4_FT_REG_FILE: u8 = 1;
    pub const EXT4_FT_DIR: u8 = 2;
    pub const EXT4_FT_CHRDEV: u8 = 3;
    pub const EXT4_FT_BLKDEV: u8 = 4;
    pub const EXT4_FT_FIFO: u8 = 5;
    pub const EXT4_FT_SOCK: u8 = 6;
    pub const EXT4_FT_SYMLINK: u8 = 7;
    
    /// 从 mode 提取文件类型
    pub fn from_mode(mode: u16) -> u8 {
        (mode >> 12) as u8
    }
    
    /// 文件类型名称
    pub fn name(t: u8) -> &'static str {
        match t {
            EXT4_FT_UNKNOWN => "unknown",
            EXT4_FT_REG_FILE => "regular",
            EXT4_FT_DIR => "directory",
            EXT4_FT_CHRDEV => "char",
            EXT4_FT_BLKDEV => "block",
            EXT4_FT_FIFO => "fifo",
            EXT4_FT_SOCK => "socket",
            EXT4_FT_SYMLINK => "symlink",
            _ => "unknown",
        }
    }
}

/// ext4 inode 标志
pub mod inode_flags {
    pub const EXT4_SECRM_FL: u32 = 0x00000001;      // Secure deletion
    pub const EXT4_UNRM_FL: u32 = 0x00000002;       // Undelete
    pub const EXT4_COMPR_FL: u32 = 0x00000004;      // Compress
    pub const EXT4_SYNC_FL: u32 = 0x00000008;       // Synchronous updates
    pub const EXT4_IMMUTABLE_FL: u32 = 0x00000010;  // Immutable
    pub const EXT4_APPEND_FL: u32 = 0x00000020;     // Append only
    pub const EXT4_NODUMP_FL: u32 = 0x00000040;     // No dump
    pub const EXT4_NOATIME_FL: u32 = 0x00000080;    // No atime
    pub const EXT4_DIRTY_FL: u32 = 0x00000100;      // Dirty
    pub const EXT4_COMPRBLK_FL: u32 = 0x00000200;   // Compressed blocks
    pub const EXT4_NOCOMPR_FL: u32 = 0x00000400;    // Don't compress
    pub const EXT4_ENCRYPT_FL: u32 = 0x00000800;    // Encrypted
    pub const EXT4_INDEX_FL: u32 = 0x00001000;      // Hash indexed directory
    pub const EXT4_IMAGIC_FL: u32 = 0x00002000;      // AFS directory
    pub const EXT4_JOURNAL_DATA_FL: u32 = 0x00004000; // Journal file data
    pub const EXT4_NOTAIL_FL: u32 = 0x00008000;      // File tail should not be merged
    pub const EXT4_DIRSYNC_FL: u32 = 0x00010000;     // Synchronous directory updates
    pub const EXT4_TOPDIR_FL: u32 = 0x00020000;      // Top of directory hierarchy
    pub const EXT4_HUGE_FILE_FL: u32 = 0x00040000;   // Huge file
    pub const EXT4_EXTENTS_FL: u32 = 0x00080000;     // Inode uses extents
    pub const EXT4_VERITY_FL: u32 = 0x00100000;      // Verity protected file
    pub const EXT4_EA_INODE_FL: u32 = 0x00200000;    // EA inode
    pub const EXT4_EOFBLOCKS_FL: u32 = 0x00400000;  // Blocks allocated beyond EOF
    pub const EXT4_SNAPFILE_FL: u32 = 0x01000000;    // Inode is a snapshot
    pub const EXT4_INLINE_DATA_FL: u32 = 0x10000000; // Inline data
    pub const EXT4_PROJINHERIT_FL: u32 = 0x20000000; // Project hierarchy
    pub const EXT4_RESERVED_FL: u32 = 0x80000000;    // Reserved for ext4 lib
}

/// ext4 extent 树状态
pub mod extent_status {
    pub const EXTENT_STATUS_INIT: u8 = 0;
    pub const EXTENT_STATUS_LOADING: u8 = 1;
    pub const EXTENT_STATUS_LOADED: u8 = 2;
    pub const EXTENT_STATUS_UNWRITTEN: u8 = 3;
    pub const EXTENT_STATUS_HOLE: u8 = 4;
}
