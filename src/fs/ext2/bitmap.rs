//! ext2 位图操作
//! 
//! 参考 linux-1.0.9/fs/ext2/balloc.c 和 fs/ext2/ialloc.c

/// 位图中每个块可管理的块数
pub const BITS_PER_BLOCK: usize = 4096 * 8;

/// Block bitmap operations placeholder
pub struct BlockBitmap {
    data: *const u8,
    bits: usize,
}

impl BlockBitmap {
    pub fn new(data: *const u8, bits: usize) -> Self {
        Self { data, bits }
    }
    
    pub fn test_bit(&self, bit: usize) -> bool {
        if bit >= self.bits { return false; }
        let byte = bit / 8;
        let mask = 1u8 << (bit % 8);
        // SAFETY: caller guarantees data is valid
        unsafe { (*self.data.add(byte)) & mask != 0 }
    }
}

/// Inode bitmap operations placeholder
pub struct InodeBitmap {
    data: *const u8,
    bits: usize,
}

impl InodeBitmap {
    pub fn new(data: *const u8, bits: usize) -> Self {
        Self { data, bits }
    }
    
    pub fn test_bit(&self, bit: usize) -> bool {
        if bit >= self.bits { return false; }
        let byte = bit / 8;
        let mask = 1u8 << (bit % 8);
        // SAFETY: caller guarantees data is valid
        unsafe { (*self.data.add(byte)) & mask != 0 }
    }
}
