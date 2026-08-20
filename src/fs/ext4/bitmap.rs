//! ext4 块/Inode 位图操作
//!
//! ext4 位图在块组描述符的 bg_block_bitmap/bg_inode_bitmap 块中。
//! 每个块组有独立的块位图和 inode 位图。

use crate::fs::buffer;
use crate::fs::ext4::super_block::Ext4SuperBlock;
use crate::fs::ext4::group_desc::Ext4GroupDesc;

/// 空闲计数变了 → 标记超级块脏，让 sync_supers 调用 ext2 write_super 回写
/// 超级块与组描述符里的 free_blocks_count / free_inodes_count。
fn mark_sb_dirty(dev: u16) {
    let n = unsafe { crate::fs::super_block::get_super(dev) };
    if n != buffer::NIL {
        // SAFETY: n 是 get_super 返回的有效 super 表下标。
        unsafe { (*crate::fs::super_block::sb_ptr(n)).s_dirt = true; }
    }
}

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
/// `dev` 是本文件系统所在设备号（原实现硬编码 0x0101=ramdisk，导致写 IDE
/// 上的 ext2 根文件系统时位图读到错误设备/挂死）。
pub fn alloc_block(sb: &Ext4SuperBlock, gd: &mut Ext4GroupDesc, group: u32, dev: u16) -> Option<u64> {
    let fs_block_size = sb.block_size();
    let bitmap_block = gd.block_bitmap;
    if bitmap_block == 0 { return None; }
    let blocks_per_group = sb.s_blocks_per_group as usize;
    // gd.block_bitmap 已被 read_super_full 缩放到 1024 字节块号，勿再乘 scale。
    let nchunks = (fs_block_size + 1023) / 1024;
    let bits_per_chunk = 1024 * 8;

    for chunk in 0..nchunks {
        let bn = unsafe { buffer::bread(dev, bitmap_block as u32 + chunk as u32, 1024) };
        let bn = bn?;
        let bm = unsafe { buffer::bh(bn).data() };
        let base = chunk * bits_per_chunk;
        let mut hit = None;
        for pos in 0..bits_per_chunk {
            let bit = base + pos;
            if bit == 0 || bit >= blocks_per_group { continue; }
            if !bitmap_test(bm, pos) {
                hit = Some((bit, pos));
                break;
            }
        }
        match hit {
            Some((bit, pos)) => {
                unsafe {
                    let bm_mut = buffer::bh(bn).data_mut();
                    bitmap_set(bm_mut, pos);
                }
                unsafe { buffer::mark_buffer_dirty(bn); }
                unsafe { buffer::brelse(bn); }
                gd.free_blocks_count = gd.free_blocks_count.saturating_sub(1);
                mark_sb_dirty(dev);
                // 返回文件系统块号（fs_block_size 单位）。bit 是块组内文件系统
                // 块号，group*blocks_per_group+bit 即全局文件系统块号。
                let global_block = group as u64 * blocks_per_group as u64 + bit as u64;
                return Some(global_block);
            }
            None => unsafe { buffer::brelse(bn); },
        }
    }
    None
}

/// 释放一个块，将其标记为空闲。
pub fn free_block(sb: &Ext4SuperBlock, gd: &mut Ext4GroupDesc, group: u32, block_offset: u32, dev: u16) {
    let _ = (sb, group);
    let bitmap_block = gd.block_bitmap;
    if bitmap_block == 0 { return; }
    // block_offset 是块组内的文件系统块号，与位图位一一对应。
    let bits_per_chunk = 1024 * 8;
    let bit = block_offset as usize;
    let chunk = bit / bits_per_chunk;
    let pos = bit % bits_per_chunk;

    if let Some(bn) = unsafe { buffer::bread(dev, bitmap_block as u32 + chunk as u32, 1024) } {
        unsafe {
            let bm_mut = buffer::bh(bn).data_mut();
            bitmap_clear(bm_mut, pos);
        }
        unsafe { buffer::mark_buffer_dirty(bn); }
        unsafe { buffer::brelse(bn); }
        gd.free_blocks_count = gd.free_blocks_count.saturating_add(1);
        mark_sb_dirty(dev);
    }
}

/// 在块组中分配一个空闲 inode。返回 inode 号（全局），失败返回 0。
pub fn alloc_inode(sb: &Ext4SuperBlock, gd: &mut Ext4GroupDesc, group: u32, dev: u16) -> Option<u32> {
    let fs_block_size = sb.block_size();
    let bitmap_block = gd.inode_bitmap;
    if bitmap_block == 0 { return None; }
    let inodes_per_group = sb.s_inodes_per_group as usize;
    // 位图在盘上占一个文件系统块（fs_block_size 字节），而缓冲缓存固定 1024
    // 字节一块。注意：gd.inode_bitmap 已经被 read_super_full 按 scale 缩放到
    // 1024 字节块号，这里不能再乘 scale（会双重缩放）。
    let nchunks = (fs_block_size + 1023) / 1024;
    let bits_per_chunk = 1024 * 8;

    for chunk in 0..nchunks {
        let bn = unsafe { buffer::bread(dev, bitmap_block as u32 + chunk as u32, 1024) };
        let bn = bn?;
        let bm = unsafe { buffer::bh(bn).data() };
        let base = chunk * bits_per_chunk;
        let mut hit = None;
        for pos in 0..bits_per_chunk {
            let bit = base + pos;
            if bit == 0 || bit >= inodes_per_group { continue; }
            if !bitmap_test(bm, pos) {
                hit = Some((bit, pos));
                break;
            }
        }
        match hit {
            Some((bit, pos)) => {
                unsafe {
                    let bm_mut = buffer::bh(bn).data_mut();
                    bitmap_set(bm_mut, pos);
                }
                unsafe { buffer::mark_buffer_dirty(bn); }
                unsafe { buffer::brelse(bn); }
                gd.free_inodes_count = gd.free_inodes_count.saturating_sub(1);
                mark_sb_dirty(dev);
                let global_ino = group * inodes_per_group as u32 + bit as u32 + 1;
                return Some(global_ino);
            }
            None => unsafe { buffer::brelse(bn); },
        }
    }
    None
}

/// 释放一个 inode。
pub fn free_inode(sb: &Ext4SuperBlock, gd: &mut Ext4GroupDesc, group: u32, inode_offset: u32, dev: u16) {
    let _ = (sb, group);
    let bitmap_block = gd.inode_bitmap;
    if bitmap_block == 0 { return; }
    let bits_per_chunk = 1024 * 8;
    let bit = inode_offset as usize;
    let chunk = bit / bits_per_chunk;
    let pos = bit % bits_per_chunk;

    if let Some(bn) = unsafe { buffer::bread(dev, bitmap_block as u32 + chunk as u32, 1024) } {
        unsafe {
            let bm_mut = buffer::bh(bn).data_mut();
            bitmap_clear(bm_mut, pos);
        }
        unsafe { buffer::mark_buffer_dirty(bn); }
        unsafe { buffer::brelse(bn); }
        gd.free_inodes_count = gd.free_inodes_count.saturating_add(1);
        mark_sb_dirty(dev);
    }
}
