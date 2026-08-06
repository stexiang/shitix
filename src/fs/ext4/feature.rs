//! ext4 特性定义
//! 
//! ext4 引入了多种新特性，本模块定义了这些特性的常量。

/// ext4 特性
pub struct Ext4Features;

/// ext4 只读兼容特性
pub mod ro_compat {
    /// Sparse superblock
    pub const SPARSE_SUPER: u32 = 0x0001;
    /// 64-bit file size
    pub const LARGE_FILE: u32 = 0x0002;
    /// Btree directory indexing
    pub const BTREE_DIR: u32 = 0x0004;
    /// Huge file support
    pub const HUGE_FILE: u32 = 0x0008;
    /// Group descriptor table does not use checksums
    pub const GDT_CSUM: u32 = 0x0010;
    /// Flexible block groups
    pub const FLEX_BG: u32 = 0x0020;
    /// Inodes can be used to store extended attributes
    pub const EA_INODE: u32 = 0x0040;
    /// Directory entries store file type
    pub const DIR_NLINK: u32 = 0x0100;
    /// inode has a separate checksum
    pub const INODE_CSUM: u32 = 0x0200;
    /// Extended attribute uses separate inode
    pub const EXTRA_ISIZE: u32 = 0x0400;
    /// Quota support
    pub const QUOTA: u32 = 0x0800;
    /// Project ID supported
    pub const PROJECT: u32 = 0x1000;
}

/// ext4 不兼容特性
pub mod incompat {
    /// Compression
    pub const COMPRESSION: u32 = 0x0001;
    /// Directory entries contain file type
    pub const FILETYPE: u32 = 0x0002;
    /// Needs recovery
    pub const RECOVER: u32 = 0x0004;
    /// Journal device
    pub const JOURNAL_DEV: u32 = 0x0008;
    /// Meta block groups
    pub const META_BG: u32 = 0x0010;
    /// File extent tree
    pub const EXTENTS: u32 = 0x0040;
    /// 64-bit filesystem
    pub const MMETADATA: u32 = 0x0080;
    /// Journal checksums
    pub const JOURNAL_CHECKSUM: u32 = 0x0400;
    /// Replay only (fonts-only FS)
    pub const REPLAY: u32 = 0x0800;
    /// 32-bit group numbers
    pub const BIGALLOC: u32 = 0x1000;
    /// Metadata checksums
    pub const METADATA_CSUM: u32 = 0x2000;
    /// Extended attribute uses separate data block
    pub const FAST_COMMIT: u32 = 0x4000;
    /// Inline data
    pub const INLINE_DATA: u32 = 0x8000;
    /// Encryption
    pub const ENCRYPT: u32 = 0x10000;
    /// Case-insensitive directory indexing
    pub const CASEFOLD: u32 = 0x20000;
}

/// ext4 兼容特性
pub mod compat {
    /// Directory preallocation
    pub const DIR_PREALLOC: u32 = 0x0001;
    /// Imagic inodes
    pub const IMAGIC_INODES: u32 = 0x0002;
    /// Has journal
    pub const JOURNAL: u32 = 0x0004;
    /// Extended attributes
    pub const EXT_ATTR: u32 = 0x0008;
    /// Reserved space for resize
    pub const RESIZE_INODE: u32 = 0x0010;
    /// Directory indexing
    pub const DIR_INDEX: u32 = 0x0020;
}

/// 挂载选项
pub mod mount_opts {
    /// View read-only compat features
    pub const DEBUG: u32 = 0x0001;
    /// Ignore errors
    pub const ERRORS_CONT: u32 = 0x0002;
    /// Remount read-only on errors
    pub const ERRORS_RO: u32 = 0x0004;
    /// Panic on errors
    pub const ERRORS_PANIC: u32 = 0x0008;
    /// Force errors mask
    pub const ERRORS_MASK: u32 = 0x000E;
    /// Minix-dax behavior
    pub const MINIX_DF: u32 = 0x0040;
    /// Don't write access times
    pub const NOATIME: u32 = 0x0080;
    /// Use i_version
    pub const IVERSION: u32 = 0x0100;
    /// Flush writes more often
    pub const DIOREAD_NOLOCK: u32 = 0x1000;
    /// Enable automatic checksumming
    pub const CHECK: u32 = 0x2000;
    /// Disable extent format
    pub const NOEXTENTS: u32 = 0x4000;
    /// Don't write i_mode times
    pub const NODIRATIME: u32 = 0x8000;
    /// Don't write mtime
    pub const NOMGMTIME: u32 = 0x10000;
    /// Disable block valid check
    pub const BLOCK_VALIDITY: u32 = 0x20000;
    /// Use delayed allocation
    pub const DELALLOC: u32 = 0x40000;
    /// Discard blocks
    pub const DISCARD: u32 = 0x80000;
    /// No write ordering
    pub const NODELALLOC: u32 = 0x100000;
    /// Skip orphan cleanup
    pub const INIT_ISIZE: u32 = 0x200000;
    /// Enable extents
    pub const EXTENTS: u32 = 0x400000;
    /// Use i_size > 2^32
    pub const LARGE_MMAP: u32 = 0x800000;
    /// Huge file support
    pub const HUGEMMAP: u32 = 0x1000000;
    /// Enable metadata checksums
    pub const METADATA_CSUM: u32 = 0x2000000;
    /// Enable fast commits
    pub const FAST_COMMIT: u32 = 0x4000000;
    /// Don't optimize for large files
    pub const NOLARGEIO: u32 = 0x8000000;
    /// Inode version tracking
    pub const INODE_VERSION: u32 = 0x10000000;
}

/// 日志特性
pub mod journal_feature {
    /// Incompat: Checksums
    pub const INCOMPAT_CSUM: u32 = 0x0001;
    /// Incompat: 64-bit journal size
    pub const INCOMPAT_64BIT: u32 = 0x0002;
    /// Incompat: Flexible block size
    pub const INCOMPAT_FLEX_BLK_SIZE: u32 = 0x0004;
    /// RO compat: Sparse super v2
    pub const RO_COMPAT_SPARSESUPER2: u32 = 0x0001;
}

impl Ext4Features {
    /// 检查是否支持 extent
    #[inline]
    pub fn has_extent(incompat: u32) -> bool {
        (incompat & incompat::EXTENTS) != 0
    }
    
    /// 检查是否支持 64 位
    #[inline]
    pub fn has_64bit(incompat: u32) -> bool {
        (incompat & incompat::MMETADATA) != 0
    }
    
    /// 检查是否支持大文件
    #[inline]
    pub fn has_large_file(ro_compat: u32) -> bool {
        (ro_compat & ro_compat::LARGE_FILE) != 0
    }
    
    /// 检查是否使用 journal
    #[inline]
    pub fn has_journal(compat: u32) -> bool {
        (compat & compat::JOURNAL) != 0
    }
    
    /// 检查是否支持 metadata checksum
    #[inline]
    pub fn has_metadata_csum(incompat: u32) -> bool {
        (incompat & incompat::METADATA_CSUM) != 0
    }
    
    /// 检查是否支持 inline data
    #[inline]
    pub fn has_inline_data(incompat: u32) -> bool {
        (incompat & incompat::INLINE_DATA) != 0
    }
    
    /// 检查是否支持 fast commit
    #[inline]
    pub fn has_fast_commit(incompat: u32) -> bool {
        (incompat & incompat::FAST_COMMIT) != 0
    }
    
    /// 检查是否支持 flex_bg
    #[inline]
    pub fn has_flex_bg(ro_compat: u32) -> bool {
        (ro_compat & ro_compat::FLEX_BG) != 0
    }
}
