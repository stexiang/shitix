//! 块设备层。对应 linux-1.0.9 的 `drivers/block/`。

pub mod ll_rw;
pub mod ramdisk;

pub use ll_rw::{
    Request, blk_size, ll_rw_block, register_request_fn, set_blk_size, set_device_ro,
};

/// 主设备号。对应原版 `include/linux/major.h`。
/// 只列出我们用到的和留作占位的那几个，数值与原版一致。
pub mod major {
    pub const UNNAMED_MAJOR: u32 = 0;
    /// 内存类设备：`/dev/ram` 是 (1,1)、`/dev/mem` 是 (1,1) 的字符版
    pub const MEM_MAJOR: u32 = 1;
    pub const FLOPPY_MAJOR: u32 = 2;
    pub const HD_MAJOR: u32 = 3;
    pub const TTY_MAJOR: u32 = 4;
    pub const TTYAUX_MAJOR: u32 = 5;
    pub const LP_MAJOR: u32 = 6;
}

/// 块设备表的容量。对应原版 `major.h` 的 `MAX_BLKDEV 32`。
pub const MAX_BLKDEV: usize = 32;
/// 字符设备表的容量。对应原版 `MAX_CHRDEV 32`。
pub const MAX_CHRDEV: usize = 32;

/// 一个扇区 512 字节。原版 `blk.h` 里到处是 `<< 9`。
pub const SECTOR_SIZE: usize = 512;
/// 对应原版 `blk.h` 的 `SECTOR_MASK ((BLOCK_SIZE / 512) - 1)`。
pub const SECTOR_MASK: u32 = (crate::fs::BLOCK_SIZE / SECTOR_SIZE) as u32 - 1;
