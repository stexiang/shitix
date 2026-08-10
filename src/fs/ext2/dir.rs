//! ext2 目录操作
//! 
//! 参考 linux-1.0.9/fs/ext2/dir.c
//! 
//! TODO: 实现目录遍历和搜索

// Directory entry constants
pub const EXT2_DIR_ENTRY_MIN_SIZE: usize = 8;

/// 计算目录项占用的空间
pub fn ext2_rec_len(name_len: u16) -> u16 {
    let mut rec_len = 8 + name_len;
    rec_len = (rec_len + 3) & !3u16;
    rec_len
}
