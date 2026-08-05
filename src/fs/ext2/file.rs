//! ext2 文件操作
//! 
//! 参考 linux-1.0.9/fs/ext2/file.c
//! 
//! TODO: 实现文件读写

use super::inode::Ext2Inode;

/// 从 inode 获取块号
pub fn inode_get_block(inode: &Ext2Inode, block: u32, blocksize: usize) -> Option<u32> {
    if (block as usize) < 12 {
        let b = inode.i_block[block as usize];
        if b == 0 { None } else { Some(b) }
    } else {
        None // TODO: 实现间接块查找
    }
}
