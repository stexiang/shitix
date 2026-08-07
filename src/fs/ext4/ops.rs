//! ext4 超级块检测 + VFS 操作桩（委托 minix）

use crate::fs::buffer;
use crate::fs::inode::{self, FsType, NIL};

/// ext4 read_super — 检测 ext4 魔数并初始化超级块
pub fn read_super(n: usize, _silent: bool) -> bool {
    // ext4 superblock is at logical block 0, offset 1024
    let dev = unsafe { (*crate::fs::super_block::sb_ptr(n)).s_dev };
    if dev == 0 { return false; }

    let bn = unsafe { buffer::bread(dev, 1, 1024) }; // ext4 superblock at block 1 (offset 1024)
    let bn = match bn { Some(b) => b, None => return false };

    let data = unsafe { buffer::bh(bn).data() };
    // Check ext4 magic at offset 0x38 (56) within the superblock
    if data.len() < 58 || data[56] != 0x53 || data[57] != 0xEF {
        unsafe { buffer::brelse(bn); }
        return false;
    }

    // Parse superblock
    let block_size: usize = 1024 << (data[24] as usize);
    // blocks_per_group at offset 32 (u32 LE)
    let bpg = u32::from_le_bytes([data[32], data[33], data[34], data[35]]) as usize;

    crate::pr_info!("ext4: found valid superblock on dev {:04x}, {}B blocks", dev, block_size);

    unsafe {
        let s = crate::fs::super_block::sb_ptr(n);
        (*s).s_magic = 0xEF53u32; // EXT4 magic
        (*s).s_blocksize = block_size as u32;
        (*s).s_blocksize_bits = (data[24] + 10) as u8;
        // minix 兼容字段（ext4 不用，但 minix ops 会读）
        (*s).s_dirsize = 32; // 避免 super_block check_mounted assert
        (*s).s_namelen = 30;

        // Root inode (2)
        let ip = inode::iget(n, 2); // sb slot = n, root inode = 2
        if ip == NIL { unsafe { buffer::brelse(bn); } return false; }
        let i = inode::inode(ip);
        i.i_mode = 0o40755;
        i.i_nlink = 2;
        i.i_size = 1024; // root dir is 1024 bytes
        i.i_op = FsType::Ext2;
        i.i_sb = n;
        i.i_flags = 0;
        i.data = [0; 9];
        // 从 inode 表读取 extent 数据，解析第一个数据块
        // 根 inode(2) 在 inode 表块 5 的偏移 256
        if let Some(ibn) = buffer::bread(dev, 5, 1024) {
            let idata = buffer::bh(ibn).data();
            let off = 256usize; // inode 2 at offset 256
            // Extent header at inode offset 40 (i_block[0..11])
            let ext_off = off + 40;
            if ext_off + 12 <= idata.len() {
                let magic = u16::from_le_bytes([idata[ext_off], idata[ext_off + 1]]);
                if magic == 0xF30A {
                    let entries = idata[ext_off + 2] as usize;
                    // First extent at ext_off + 12
                    let e_off = ext_off + 12;
                    if e_off + 12 <= idata.len() && entries > 0 {
                        let ee_block = u32::from_le_bytes([idata[e_off], idata[e_off + 1], idata[e_off + 2], idata[e_off + 3]]);
                        // let ee_len = u16::from_le_bytes([idata[e_off + 4], idata[e_off + 5]]);
                        let ee_start_lo = u32::from_le_bytes([idata[e_off + 8], idata[e_off + 9], idata[e_off + 10], idata[e_off + 11]]);
                        i.data[0] = ee_start_lo as u16;
                        i.i_size = 1024; // one block
                    }
                }
            }
            buffer::brelse(ibn);
        }

        (*s).s_mounted = ip;
        (*s).s_covered = NIL;
    }

    unsafe { buffer::brelse(bn); }
    true
}

/// 初始化（注册日志等）
pub fn init() {
    crate::sprintln!("ext4: VFS operations initialized (delegated to minix)");
}
