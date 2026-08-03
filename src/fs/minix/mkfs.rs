//! 在 ramdisk 上铺一个空的 minix v1 文件系统。
//!
//! **原版没有这个模块。** 原版的根文件系统是外部用 `mkfs.minix` 造好的
//! 映像，由 `drivers/block/ramdisk.c` 的 `rd_load()` 从软驱读进内存
//! （或者根本不用 ramdisk，直接挂软驱/IDE 盘）。我们既没有软驱驱动
//! 也没有外部映像可读，所以在内存里现造一个。
//!
//! 这个模块直接按 `include/linux/minix_fs.h` 描述的磁盘格式写字节，
//! 走的是 [`crate::drivers::block::ramdisk::raw_write_block`]（绕过缓冲
//! 缓存），因此它是对 [`super::inode_ops`] 那些读取代码的**独立**验证：
//! 两边都从同一份格式文档写出来，但没有共用代码，布局理解错了不会
//! 同时错成一样。
//!
//! # 布局计算
//!
//! 给定 `nblocks`（总块数）和 `ninodes`（inode 数）：
//! - 块 0：引导块，全零
//! - 块 1：超级块
//! - 块 2 起：inode 位图，`ceil(ninodes+1 / 8192)` 块
//! - 接着：zone 位图，`ceil(nblocks / 8192)` 块
//! - 接着：inode 表，`ceil(ninodes / 32)` 块
//! - `firstdatazone` 起：数据区
//!
//! 造完之后根目录（inode 1）里有 `.` 和 `..` 两项，都指向自己。

use crate::drivers::block::ramdisk;
use crate::fs::BLOCK_SIZE;
use crate::fs::mode;
use crate::pr_info;

use super::{
    BITS_PER_BLOCK, MINIX_INODES_PER_BLOCK, MINIX_ROOT_INO, MINIX_SUPER_MAGIC, MINIX_VALID_FS,
};

/// 一个块的暂存区。造文件系统时逐块拼好再写下去。
///
/// **不要把它放在栈上。** `mkfs` 在内核线程里跑，栈是静态池里的
/// [`crate::sched::KSTACK_SIZE`] 字节；每个 `Block` 是 1KB，加上
/// `raw_write_block` 的调用链和随时可能插进来的中断栈帧，几个局部
/// `Block` 就能把栈顶推出去。溢出踩的是 `.bss` 里排在栈之前的东西
/// （`fs::buffer::BUFFERS`、`fs::super_block::SUPER_BLOCKS`），症状是
/// 超级块字段变成低端内存 BIOS ROM 的字节（`s_dev=0xff53`、
/// `ninodes=61440`），完全看不出源头。所以统一用 [`scratch`] 那一份
/// 静态暂存区。
struct Block {
    data: [u8; BLOCK_SIZE],
}

/// 唯一的静态块暂存区。见 [`Block`] 的说明。
static mut SCRATCH: Block = Block::__ZERO;

/// 取静态暂存区并清零。等价于原来的 `Block::new()`，但不占栈。
///
/// # Safety
/// `mkfs` 是启动期单线程执行的，同一时刻只有一处在用这块暂存区；
/// 返回的引用不得跨越下一次 `scratch()` 调用。
unsafe fn scratch() -> &'static mut Block {
    // SAFETY: 契约保证独占；SCRATCH 是地址恒定的静态变量。
    unsafe {
        let b = &mut *core::ptr::addr_of_mut!(SCRATCH);
        b.data.fill(0);
        b
    }
}

impl Block {
    /// 全零的初值，只给 [`SCRATCH`] 的静态初始化用。
    const __ZERO: Block = Block { data: [0; BLOCK_SIZE] };

    fn put_u16(&mut self, off: usize, v: u16) {
        self.data[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u32(&mut self, off: usize, v: u32) {
        self.data[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// 置第 `n` 位。位序同 [`super::bitmap`]（低位在前）。
    fn set_bit(&mut self, n: u32) {
        let byte = (n / 8) as usize;
        if byte < BLOCK_SIZE {
            self.data[byte] |= 1 << (n % 8);
        }
    }
}

/// 造好的文件系统的布局参数。造完返回给调用方打印/自检。
pub struct Layout {
    pub nblocks: u32,
    pub ninodes: u16,
    pub imap_blocks: u16,
    pub zmap_blocks: u16,
    pub inode_blocks: u32,
    pub firstdatazone: u16,
}

/// 在 ramdisk 上铺一个空的 minix v1 文件系统。
///
/// `nblocks` 是设备总块数，`ninodes` 是 inode 数（会向上取整到
/// 整块的倍数，同 `mkfs.minix` 的做法）。
///
/// # Safety
/// 启动期调用，必须在 [`ramdisk::init`] 之后、`mount_root` 之前，
/// 且此时缓冲缓存里不能有这个设备的任何块（否则缓存与盘上内容不一致）。
pub unsafe fn mkfs(nblocks: u32, ninodes: u16) -> Option<Layout> {
    if !ramdisk::is_ready() {
        return None;
    }
    // inode 数向上取整到整块（mkfs.minix 也这样）
    let ninodes = {
        let per = MINIX_INODES_PER_BLOCK as u32;
        let n = ((ninodes as u32 + per - 1) / per) * per;
        n.min(u16::MAX as u32) as u16
    };

    // 位图块数。注意 inode 位图要多算一位（第 0 位是占位的“不存在的
    // inode 0”），zone 位图同理多一位，所以都是 +1 再向上取整。
    let imap_blocks = ((ninodes as u32 + 1 + BITS_PER_BLOCK - 1) / BITS_PER_BLOCK) as u16;
    let zmap_blocks = ((nblocks + 1 + BITS_PER_BLOCK - 1) / BITS_PER_BLOCK) as u16;
    let inode_blocks = (ninodes as u32 + MINIX_INODES_PER_BLOCK - 1) / MINIX_INODES_PER_BLOCK;
    let firstdatazone = (2 + imap_blocks as u32 + zmap_blocks as u32 + inode_blocks) as u16;

    if firstdatazone as u32 >= nblocks {
        return None;
    }

    // SAFETY: 契约保证 ramdisk 已就绪、无并发访问、缓存里没有本设备的块。
    unsafe {
        // ---- 块 0：引导块 ----
        ramdisk::raw_write_block(0, &scratch().data);

        // ---- 块 1：超级块 ----
        let sb = scratch();
        sb.put_u16(0, ninodes);
        sb.put_u16(2, nblocks as u16);
        sb.put_u16(4, imap_blocks);
        sb.put_u16(6, zmap_blocks);
        sb.put_u16(8, firstdatazone);
        // log_zone_size = 0：一个 zone 就是一个块（minix v1 的常规值）
        sb.put_u16(10, 0);
        // s_max_size：7 直接 + 512 一级 + 512*512 二级，乘块大小
        let max_blocks = super::DIRECT_ZONES as u32
            + super::ZONES_PER_BLOCK
            + super::ZONES_PER_BLOCK * super::ZONES_PER_BLOCK;
        sb.put_u32(12, max_blocks.saturating_mul(BLOCK_SIZE as u32));
        sb.put_u16(16, MINIX_SUPER_MAGIC as u16);
        sb.put_u16(18, MINIX_VALID_FS);
        ramdisk::raw_write_block(1, &sb.data);
        // 写完立刻读回校验魔数。这一步不是多余的：mount 失败时唯一的
        // 症状是 read_super 报「找不到 minix 文件系统」，而那既可能是
        // mkfs 没写对，也可能是读路径（请求队列/缓冲缓存）串了。在这里
        // 卡一刀，把两者分开。
        let mut back = [0u8; 1024];
        if !ramdisk::raw_read_block(1, &mut back) || back[16..18] != sb.data[16..18] {
            crate::pr_err!(
                "mkfs.minix: superblock readback mismatch (wrote {:#06x}, read {:#06x})",
                u16::from_le_bytes([sb.data[16], sb.data[17]]),
                u16::from_le_bytes([back[16], back[17]])
            );
            return None;
        }

        // ---- inode 位图 ----
        // 第 0 位（不存在的 inode 0）与第 1 位（根目录）都占用
        for i in 0..imap_blocks as usize {
            let b = scratch();
            if i == 0 {
                b.set_bit(0);
                b.set_bit(MINIX_ROOT_INO);
            }
            // 位图里超出 ninodes 的那些位要置 1，否则 new_inode 会
            // 派出不存在的 inode 号。mkfs.minix 同样这么做。
            let base = i as u32 * BITS_PER_BLOCK;
            for bit in 0..BITS_PER_BLOCK {
                if base + bit > ninodes as u32 {
                    b.set_bit(bit);
                }
            }
            ramdisk::raw_write_block(2 + i, &b.data);
        }

        // ---- zone 位图 ----
        // 位号 n 对应块号 firstdatazone + n - 1（见 bitmap.rs 的偏移说明）。
        // 第 0 位占位；根目录的数据块占第 1 位。
        let zmap_start = 2 + imap_blocks as usize;
        for i in 0..zmap_blocks as usize {
            let b = scratch();
            if i == 0 {
                b.set_bit(0);
                b.set_bit(1); // 根目录的数据块
            }
            // 超出设备容量的位置 1
            let base = i as u32 * BITS_PER_BLOCK;
            let max_bit = nblocks - firstdatazone as u32 + 1;
            for bit in 0..BITS_PER_BLOCK {
                if base + bit >= max_bit {
                    b.set_bit(bit);
                }
            }
            ramdisk::raw_write_block(zmap_start + i, &b.data);
        }

        // ---- inode 表 ----
        let itable_start = zmap_start + zmap_blocks as usize;
        for i in 0..inode_blocks as usize {
            ramdisk::raw_write_block(itable_start + i, &scratch().data);
        }

        // 根目录 inode（1 号，在 inode 表第 0 块的第 0 项）
        let itb = scratch();
        // i_mode: 目录 + 0755
        itb.put_u16(0, mode::S_IFDIR | 0o755);
        // i_uid = 0
        itb.put_u16(2, 0);
        // i_size：. 和 .. 两项，每项 16 字节（MINIX_SUPER_MAGIC → dirsize 16）
        itb.put_u32(4, 32);
        itb.put_u32(8, 0); // i_time
        itb.data[12] = 0; // i_gid
        itb.data[13] = 2; // i_nlinks：. 和父目录的引用（根的 .. 指向自己）
        // i_zone[0] = 第一个数据块
        itb.put_u16(14, firstdatazone);
        ramdisk::raw_write_block(itable_start, &itb.data);

        // ---- 根目录的数据块："." 与 ".." 都指向 inode 1 ----
        let root = scratch();
        root.put_u16(0, MINIX_ROOT_INO as u16);
        root.data[2] = b'.';
        root.put_u16(16, MINIX_ROOT_INO as u16);
        root.data[18] = b'.';
        root.data[19] = b'.';
        ramdisk::raw_write_block(firstdatazone as usize, &root.data);

        // ---- 其余数据块清零 ----
        let zero = scratch();
        for b in (firstdatazone as usize + 1)..nblocks as usize {
            ramdisk::raw_write_block(b, &zero.data);
        }
    }

    pr_info!(
        "mkfs.minix: {} blocks, {} inodes, imap={} zmap={} itable={} firstdata={}",
        nblocks,
        ninodes,
        imap_blocks,
        zmap_blocks,
        inode_blocks,
        firstdatazone
    );

    Some(Layout { nblocks, ninodes, imap_blocks, zmap_blocks, inode_blocks, firstdatazone })
}
