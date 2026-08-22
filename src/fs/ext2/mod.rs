//! ext2 磁盘数据结构定义。
//!
//! 历史上这里有一套独立的 ext2 实现骨架，已被 `ext4/` 目录下的完整
//! ext2/ext4 读写实现取代（ext4 无 extent 时就是 ext2 经典块布局）。
//! 现在只保留 `ext4/` 仍引用的磁盘结构定义。
//!
//! 参考 linux-1.0.9 的 `include/linux/ext2_fs.h`。

pub mod super_block;
pub mod inode;

// Re-exports for convenience
pub use super_block::{Ext2SuperBlock, Ext2GroupDesc, EXT2_SUPER_MAGIC};
pub use inode::{Ext2Inode, Ext2DirEntry};
