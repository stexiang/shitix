//! ext4 mkfs — 在内存中创建 ext4 文件系统
//!
//! 创建一个单块组、extent 特性的 ext4 文件系统。
//! 布局：
//!   块 0: 引导块 (1024B) + 超级块 (1024B)
//!   块 1: 块组描述符表
//!   块 2: 块位图
//!   块 3: inode 位图
//!   块 4-11: inode 表 (8 blocks, 32 inodes)
//!   块 12-: 数据区 (根目录在块 12)

use crate::drivers::block::ramdisk;
use crate::fs::BLOCK_SIZE;

/// 创建 ext4 文件系统。参数：总块数、每 inode 块数。
pub unsafe fn mkfs(total_blocks: u32, _inode_ratio: u32) -> bool {
    // ext4 使用 1024 字节块
    const BS: usize = 1024;
    let bpg: u32 = total_blocks; // 单块组：所有块在一个组里

    crate::sprintln!("mkfs.ext4: {} blocks, {}K", total_blocks, total_blocks);

    // --- 块 0: 引导块 + 超级块 ---
    let mut blk0 = [0u8; 2048];
    // 超级块从偏移 1024 开始
    let sb = &mut blk0[1024..];
    // s_inodes_count
    sb[0..4].copy_from_slice(&32u32.to_le_bytes());
    // s_blocks_count_lo
    sb[4..8].copy_from_slice(&total_blocks.to_le_bytes());
    // s_r_blocks_count_lo
    sb[8..12].copy_from_slice(&0u32.to_le_bytes());
    // s_free_blocks_count_lo
    sb[12..16].copy_from_slice(&((total_blocks - 12) as u32).to_le_bytes());
    // s_free_inodes_count
    sb[16..20].copy_from_slice(&21u32.to_le_bytes()); // 32 - 11 reserved
    // s_first_data_block = 0
    // s_log_block_size = 0 (1024 bytes)
    // s_blocks_per_group (offset 32)
    sb[32..36].copy_from_slice(&bpg.to_le_bytes());
    // s_inodes_per_group (offset 40)
    sb[40..44].copy_from_slice(&32u32.to_le_bytes());
    // s_magic (offset 56)
    sb[56] = 0x53; sb[57] = 0xEF;
    // s_state = 0 (clean)
    // s_rev_level = 1 (dynamic) (offset 76)
    sb[76..80].copy_from_slice(&1u32.to_le_bytes());
    // s_inode_size = 256 (offset 88)
    sb[88] = 0; sb[89] = 1; // u16 LE = 256
    // s_feature_incompat: EXTENTS(0x40) | FILETYPE(0x2) (offset 96)
    sb[96..100].copy_from_slice(&0x42u32.to_le_bytes());
    // s_feature_ro_compat: SPARSE_SUPER(0x1) | LARGE_FILE(0x2) (offset 100)
    sb[100..104].copy_from_slice(&0x3u32.to_le_bytes());
    // s_mnt_count = 1
    sb[52..54].copy_from_slice(&1u16.to_le_bytes());
    // s_max_mnt_count = 30
    sb[54..56].copy_from_slice(&30u16.to_le_bytes());

    // Write block 0 (2 × 1024B = block 0 in 2048-byte terms... actually block 0 is 1024B)
    // In ext4 with 1024B blocks, superblock is in block 0 at offset 1024
    unsafe { ramdisk::raw_write_block(0, &blk0[..1024]) };
    unsafe { ramdisk::raw_write_block(1, &blk0[1024..]) };
    // Block 0 (first 1024B) and block 1 (second 1024B with superblock)

    // --- 块 2: 块组描述符 ---
    let mut blk2 = [0u8; BS];
    let gdoff = 0;
    // bg_block_bitmap (offset 0): block 3 (counting from 0 in 1024B blocks...
    // Actually in our layout block 2 = bitmap)
    // Let me relabel: block 0=boot, block 1=superblock, block 2=group desc, block 3=block bitmap, block 4=inode bitmap, block 5=inode table start
    // Wait, ext4 with 1024B blocks has:
    // Block 0: boot+superblock (first 1024B is boot, second 1024B is superblock in block 0)
    // Actually ext4's superblock is at offset 1024 within block group, which means it's at byte 1024 of the device. With 1024B blocks, that's block 1 (block 0 = bytes 0-1023, block 1 = bytes 1024-2047).
    // So: block 0=boot, block 1=super, block 2=group_desc, block 3=block_bitmap, block 4=inode_bitmap, block 5-12=inode_table, block 13+=data

    // bg_block_bitmap at offset 0
    gd_put32(&mut blk2, gdoff, 2);  // block 3 → wait, let me recount
    // With 1024B blocks:
    // superblock is at byte 1024 = block 1
    // Let me use 0-based group descriptor. bg_block_bitmap = block 2 (third block, zero-based = 2)
    // Actually: block 0=boot, block 1=super, block 2=desc, block 3=block_bitmap, block 4=inode_bitmap
    // nope: with 1024B blocks, boot+super fit in 2 blocks. So:
    // block 0 = boot (bytes 0-1023)
    // block 1 = superblock (bytes 1024-2047)
    // block 2 = group desc table
    // block 3 = block bitmap
    // block 4 = inode bitmap
    // block 5-12 = inode table
    // block 13+ = data blocks

    // bg_block_bitmap (offset 0): 3 (zero-based block number within filesystem)
    gd_put32(&mut blk2, gdoff + 0, 3);
    // bg_inode_bitmap (offset 4): 4
    gd_put32(&mut blk2, gdoff + 4, 4);
    // bg_inode_table (offset 8): 5 (start of inode table)
    gd_put32(&mut blk2, gdoff + 8, 5);
    // bg_free_blocks_count (offset 12)
    gd_put16(&mut blk2, gdoff + 12, (total_blocks - 13) as u16);
    // bg_free_inodes_count (offset 14)
    gd_put16(&mut blk2, gdoff + 14, 21);
    // bg_used_dirs_count (offset 16)
    gd_put16(&mut blk2, gdoff + 16, 2); // . and ..

    unsafe { ramdisk::raw_write_block(2, &blk2) };

    // --- 块 3: 块位图 ---
    let mut blk3 = [0u8; BS];
    blk3[0] = 0xFF; // bits 0-7: blocks 0-7 reserved (super, desc, bitmaps, inode table)
    blk3[1] = 0x1F; // bits 8-12: blocks 8-12 reserved (remaining inode table blocks)
    unsafe { ramdisk::raw_write_block(3, &blk3) };

    // --- 块 4: inode 位图 ---
    let mut blk4 = [0u8; BS];
    blk4[0] = 0xFF; // inodes 0-7
    blk4[1] = 0x07; // inodes 8-10 (inode 0 reserved, inodes 1-10 used by fs metadata)
    // inode 2 = root, inode 8 = journal (预留), others = lost+found etc
    // For simplicity: mark inodes 1-10 as used
    unsafe { ramdisk::raw_write_block(4, &blk4) };

    // --- 块 5-12: inode 表 (8 blocks × 4 inodes/block = 32 inodes) ---
    // Initialize root inode (inode 2) in block 5
    let mut ino_blk = [0u8; BS];
    // inode 2 is the second inode in the table, at offset 256 (inode 2 has index 1 since inode 1 is first)
    // With 256-byte inodes, block 5 has inodes 1-4 (offsets 0, 256, 512, 768)
    // inode 2 = offset 256 within block 5
    let ino2 = 256usize;
    // i_mode: directory + 0755
    ino_blk[ino2] = 0xED; ino_blk[ino2 + 1] = 0x41; // 0x41ED = drwxr-xr-x
    // i_uid = 0, i_gid = 0
    // i_size_lo: 1024 (one block)
    ino_blk[ino2 + 4] = 0x00; ino_blk[ino2 + 5] = 0x04;
    // i_links_count = 2 (. and ..)
    ino_blk[ino2 + 26] = 2;
    // i_flags: EXT4_EXTENTS_FL (0x00080000, LE bytes [0x00,0x00,0x08,0x00])
    ino_blk[ino2 + 32] = 0x00; ino_blk[ino2 + 33] = 0x00; ino_blk[ino2 + 34] = 0x08; ino_blk[ino2 + 35] = 0x00;
    // i_block[0..11] = extent header at offset 40 (12 bytes)
    // ExtentHeader layout (small-endian):
    //   byte 0-1: eh_magic = 0xF30A
    //   byte 2-3: eh_entries = 1
    //   byte 4-5: eh_max = 4
    //   byte 6-7: eh_depth = 0
    //   byte 8-11: eh_generation = 0
    let ext_off = ino2 + 40;
    ino_blk[ext_off] = 0x0A; ino_blk[ext_off + 1] = 0xF3; // magic
    ino_blk[ext_off + 2] = 1; ino_blk[ext_off + 3] = 0;    // entries = 1
    ino_blk[ext_off + 4] = 4; ino_blk[ext_off + 5] = 0;    // max_entries = 4
    ino_blk[ext_off + 6] = 0; ino_blk[ext_off + 7] = 0;    // depth = 0
    for k in 8..12 { ino_blk[ext_off + k] = 0; }            // generation = 0
    // Extent entry at ext_off + 12 (12 bytes):
    //   byte 0-3: ee_block (logical block 0)
    //   byte 4-5: ee_len (1)
    //   byte 6-7: ee_start_hi (0)
    //   byte 8-11: ee_start_lo (physical block 13)
    let ext_dat = ext_off + 12;
    ino_blk[ext_dat..ext_dat + 4].copy_from_slice(&0u32.to_le_bytes());     // ee_block = 0
    ino_blk[ext_dat + 4..ext_dat + 6].copy_from_slice(&1u16.to_le_bytes()); // ee_len = 1
    ino_blk[ext_dat + 6..ext_dat + 8].copy_from_slice(&0u16.to_le_bytes()); // ee_start_hi = 0
    ino_blk[ext_dat + 8..ext_dat + 12].copy_from_slice(&13u32.to_le_bytes()); // ee_start_lo = 13
    ino_blk[ext_dat + 6..ext_dat + 8].copy_from_slice(&0u16.to_le_bytes()); // ee_start_hi
    ino_blk[ext_dat + 8..ext_dat + 12].copy_from_slice(&13u32.to_le_bytes()); // ee_start_lo
    // i_extra_isize = 28
    ino_blk[ino2 + 128] = 28;

    unsafe { ramdisk::raw_write_block(5, &ino_blk) };

    // --- 块 13: 根目录数据块 ---
    let mut dir_blk = [0u8; BS];
    // "." entry: inode=2, name_len=1, name=".", rec_len=12
    dir_blk[0..4].copy_from_slice(&2u32.to_le_bytes()); // inode
    dir_blk[4..6].copy_from_slice(&12u16.to_le_bytes()); // rec_len
    dir_blk[6] = 1; // name_len
    dir_blk[7] = 2; // file_type = dir
    dir_blk[8] = b'.';
    // ".." entry: inode=2, name_len=2, name="..", rec_len=1012
    dir_blk[12..16].copy_from_slice(&2u32.to_le_bytes()); // inode
    let remaining = (BS - 12) as u16;
    dir_blk[16..18].copy_from_slice(&remaining.to_le_bytes()); // rec_len = rest of block
    dir_blk[18] = 2; // name_len
    dir_blk[19] = 2; // file_type = dir
    dir_blk[20] = b'.'; dir_blk[21] = b'.';

    unsafe { ramdisk::raw_write_block(13, &dir_blk) };

    crate::sprintln!("ext4: mkfs done ({} blocks, root inode 2)", total_blocks);
    true
}

fn gd_put32(blk: &mut [u8], off: usize, v: u32) {
    blk[off..off+4].copy_from_slice(&v.to_le_bytes());
}

fn gd_put16(blk: &mut [u8], off: usize, v: u16) {
    blk[off..off+2].copy_from_slice(&v.to_le_bytes());
}
