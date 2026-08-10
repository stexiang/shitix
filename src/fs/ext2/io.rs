//! ext2 低级 I/O 操作
//! 
//! TODO: 实现与缓冲区缓存的集成

/// 超级块偏移（字节）
pub const EXT2_SUPERBLOCK_OFFSET: u64 = 1024;

/// 读取超级块
pub fn read_superblock(
    _dev: u32,
    sb_out: &mut [u8; 1024],
    _read_fn: impl Fn(u32, u64, &mut [u8]) -> bool,
) -> bool {
    if sb_out.len() < 1024 {
        return false;
    }
    // TODO: 实现从块设备读取
    false
}

/// 计算校验和
pub fn ext2_crc16(seed: u16, data: &[u8]) -> u16 {
    let mut crc = seed;
    for &byte in data {
        crc = crc.wrapping_add(byte as u16);
        crc = (crc << 1) | (crc >> 15);
    }
    crc
}
