//! ext2 路径名查找
//! 
//! 参考 linux-1.0.9/fs/ext2/namei.c
//! 
//! TODO: 实现路径名查找

/// 最大路径名分量长度
pub const EXT2_NAME_LEN: usize = 255;
