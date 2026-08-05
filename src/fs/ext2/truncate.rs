//! ext2 文件截断
//! 
//! 参考 linux-1.0.9/fs/ext2/truncate.c
//! 
//! TODO: 实现文件截断

use super::inode::{Ext2Inode, EXT2_NDIR_BLOCKS};

/// 释放 inode 的所有块
pub fn ext2_truncate(
    inode: &mut Ext2Inode,
    _blocksize: usize,
    _free_block_fn: impl Fn(u32),
) {
    // 释放直接块
    for i in 0..EXT2_NDIR_BLOCKS {
        if inode.i_block[i] != 0 {
            inode.i_block[i] = 0;
        }
    }
    
    // 释放间接块
    if inode.i_block_1ind != 0 {
        inode.i_block_1ind = 0;
    }
    if inode.i_block_2ind != 0 {
        inode.i_block_2ind = 0;
    }
    if inode.i_block_3ind != 0 {
        inode.i_block_3ind = 0;
    }
    
    inode.i_blocks = 0;
    inode.i_size = 0;
}
