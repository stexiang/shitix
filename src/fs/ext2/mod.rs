//! ext2/ext3/ext4 文件系统实现
//! 
//! 参考 linux-1.0.9 的 `fs/ext2/` 目录
//! 
//! ## 状态
//! 
//! 核心数据结构已定义，需要与缓冲区缓存和 VFS 层集成。
//! 
//! ## 待完成
//! 
//! - [ ] 与 buffer cache 集成
//! - [ ] 超级块读取/解析
//! - [ ] inode 读取/写入
//! - [ ] 目录操作
//! - [ ] 块分配/释放
//! - [ ] 文件读写

pub mod super_block;
pub mod inode;
pub mod bitmap;
pub mod dir;
pub mod namei;
pub mod file;
pub mod truncate;
pub mod io;
pub mod ops;

// Re-exports for convenience
pub use super_block::{Ext2SuperBlock, Ext2GroupDesc, EXT2_SUPER_MAGIC};
pub use inode::{Ext2Inode, Ext2DirEntry, Ext2InodeInfo};
pub use bitmap::{BlockBitmap, InodeBitmap};
