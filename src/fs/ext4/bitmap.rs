//! ext4 块/Inode 位图操作
//!
//! ext4 位图在块组描述符的 bg_block_bitmap/bg_inode_bitmap 块中。
//! 每个块组有独立的块位图和 inode 位图。

use crate::fs::buffer;
use crate::fs::ext4::super_block::Ext4SuperBlock;
use crate::fs::ext4::group_desc::Ext4GroupDesc;

/// 检查位图中某一位是否被占用
fn bitmap_test(bitmap: &[u8], bit: usize) -> bool {
    let byte = bit / 8;
    let mask = 1u8 << (bit & 7);
    if byte < bitmap.len() { bitmap[byte] & mask != 0 } else { false }
}

/// 设置位图中的某一位
fn bitmap_set(bitmap: &mut [u8], bit: usize) {
    let byte = bit / 8;
    let mask = 1u8 << (bit & 7);
    if byte < bitmap.len() { bitmap[byte] |= mask; }
}

/// 清除位图中的某一位
fn bitmap_clear(bitmap: &mut [u8], bit: usize) {
    let byte = bit / 8;
    let mask = !(1u8 << (bit & 7));
    if byte < bitmap.len() { bitmap[byte] &= mask; }
}

/// 在块组位图中找一个空闲位
pub fn find_free_bit(bitmap: &[u8], max_bits: usize) -> Option<usize> {
    for bit in 1..max_bits { // bit 0 保留（超级块/组描述符）
        if !bitmap_test(bitmap, bit) {
            return Some(bit);
        }
    }
    None
}

/// 在块组中分配一个空闲块。返回块号（全局），失败返回 0。
pub fn alloc_block(sb: &Ext4SuperBlock, gd: &mut Ext4GroupDesc, group: u32) -> Option<u64> {
    let block_size = sb.block_size();
    let bitmap_block = gd.block_bitmap;
    if bitmap_block == 0 { return None; }

    let bn = unsafe { buffer::bread(0x0101, bitmap_block as u32, block_size) };
    let bn = bn?;

    let bm_data = unsafe { buffer::bh(bn).data() };
    let blocks_per_group = sb.s_blocks_per_group as usize;
    let mut found = None;

    for bit in 1..blocks_per_group {
        if !bitmap_test(bm_data, bit) {
            found = Some(bit);
            break;
        }
    }

    if let Some(bit) = found {
        // 在缓冲中设位并标记脏
        unsafe {
            let bm_mut = buffer::bh(bn).data_mut();
            bitmap_set(bm_mut, bit);
        }
        unsafe { buffer::mark_buffer_dirty(bn); }
        unsafe { buffer::brelse(bn); }

        // 更新组描述符中的空闲块计数
        gd.free_blocks_count = gd.free_blocks_count.saturating_sub(1);
        let global_block = group as u64 * blocks_per_group as u64 + bit as u64;
        Some(global_block)
    } else {
        unsafe { buffer::brelse(bn); }
        None
    }
}

/// 释放一个块，将其标记为空闲。
pub fn free_block(sb: &Ext4SuperBlock, gd: &mut Ext4GroupDesc, group: u32, block_offset: u16) {
    let block_size = sb.block_size();
    let bitmap_block = gd.block_bitmap;
    if bitmap_block == 0 { return; }

    if let Some(bn) = unsafe { buffer::bread(0x0101, bitmap_block as u32, block_size) } {
        let bit = block_offset as usize;
        unsafe {
            let bm_mut = buffer::bh(bn).data_mut();
            bitmap_clear(bm_mut, bit);
        }
        unsafe { buffer::mark_buffer_dirty(bn); }
        unsafe { buffer::brelse(bn); }
        gd.free_blocks_count = gd.free_blocks_count.saturating_add(1);
    }
}

/// 在块组中分配一个空闲 inode。返回 inode 号（全局），失败返回 0。
pub fn alloc_inode(sb: &Ext4SuperBlock, gd: &mut Ext4GroupDesc, group: u32) -> Option<u32> {
    let block_size = sb.block_size();
    let bitmap_block = gd.inode_bitmap;
    if bitmap_block == 0 { return None; }

    let bn = unsafe { buffer::bread(0x0101, bitmap_block as u32, block_size) };
    let bn = bn?;

    let bm_data = unsafe { buffer::bh(bn).data() };
    let inodes_per_group = sb.s_inodes_per_group as usize;
    let mut found = None;

    for bit in 1..inodes_per_group {
        if !bitmap_test(bm_data, bit) {
            found = Some(bit);
            break;
        }
    }

    if let Some(bit) = found {
        unsafe {
            let bm_mut = buffer::bh(bn).data_mut();
            bitmap_set(bm_mut, bit);
        }
        unsafe { buffer::mark_buffer_dirty(bn); }
        unsafe { buffer::brelse(bn); }

        gd.free_inodes_count = gd.free_inodes_count.saturating_sub(1);
        let global_ino = group * inodes_per_group as u32 + bit as u32 + 1;
        Some(global_ino)
    } else {
        unsafe { buffer::brelse(bn); }
        None
    }
}

/// 释放一个 inode。
pub fn free_inode(sb: &Ext4SuperBlock, gd: &mut Ext4GroupDesc, group: u32, inode_offset: u16) {
    let bitmap_block = gd.inode_bitmap;
    if bitmap_block == 0 { return; }
    let block_size = sb.block_size();

    if let Some(bn) = unsafe { buffer::bread(0x0101, bitmap_block as u32, block_size) } {
        let bit = inode_offset as usize;
        unsafe {
            let bm_mut = buffer::bh(bn).data_mut();
            bitmap_clear(bm_mut, bit);
        }
        unsafe { buffer::mark_buffer_dirty(bn); }
        unsafe { buffer::brelse(bn); }
        gd.free_inodes_count = gd.free_inodes_count.saturating_add(1);
    }
}
