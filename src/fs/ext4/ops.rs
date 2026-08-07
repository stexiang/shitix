//! ext4 超级块检测 + VFS 操作桩（委托 minix）

use crate::fs::buffer;
use crate::fs::inode::{self, FsType, NIL};

/// 轻量版 read_super：只检测魔数、挂载根 inode。
/// 不含块组描述符解析或 EXT4_INFO 填充。
pub fn read_super_light(n: usize, _silent: bool) -> bool {
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
        // 从磁盘读取根 inode 的第一个数据块号
        if let Some(ibn) = buffer::bread(dev, 5, 1024) {
            let idata = buffer::bh(ibn).data();
            let off = 256usize;
            let ext_off = off + 40;
            if ext_off + 12 <= idata.len() {
                let magic = u16::from_le_bytes([idata[ext_off], idata[ext_off + 1]]);
                if magic == 0xF30A {
                    let entries = idata[ext_off + 2] as usize;
                    if entries > 0 {
                        let e_off = ext_off + 12;
                        let ee_start_lo = u32::from_le_bytes([idata[e_off + 8], idata[e_off + 9], idata[e_off + 10], idata[e_off + 11]]);
                        i.data[0] = ee_start_lo as u16;
                        i.i_size = 1024;
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
    #[cfg(feature = "extra-drivers")]
    crate::sprintln!("ext4: VFS operations online (full ext4 ops)");
    #[cfg(not(feature = "extra-drivers"))]
    crate::sprintln!("ext4: VFS operations initialized (delegated to minix)");
}

/// 当 `extra-drivers` 启用时，用完整版 `read_super`（解析组描述符、填充 EXT4_INFO）
/// 替换轻量探测版。VFS 挂载路径通过这个函数入口调用。
#[cfg(feature = "extra-drivers")]
pub fn read_super(n: usize, silent: bool) -> bool {
    if full::read_super_full(n) {
        return true;
    }
    // 回退到轻量版
    read_super_light(n, silent)
}

#[cfg(not(feature = "extra-drivers"))]
pub fn read_super(n: usize, silent: bool) -> bool {
    read_super_light(n, silent)
}

// ---- 完整的 ext4 VFS 实现（仅 extra-drivers feature） ----
#[cfg(feature = "extra-drivers")]
pub mod full {
    //! 完整的 ext4 inode/file/namei 操作。
    //! 仅在 `extra-drivers` feature 启用时编译，避免 debug 镜像过大。

    use core::ptr;
    use crate::fs::buffer::{self, BLOCK_SIZE, NIL, bh};
    use crate::fs::inode::{self, FsType};
    use crate::fs::super_block::{self, sb};
    use crate::fs::{MS_RDONLY, mode, NR_SUPER};
    use crate::klib::errno;
    use crate::fs::ext4::bitmap;
    use crate::fs::ext4::group_desc::{self, Ext4GroupDesc};
    use crate::fs::ext4::super_block::Ext4SuperBlock;
    use crate::fs::ext4::inode::Ext4Inode;
    use crate::fs::ext4::extent::{ExtentNode, EXT_INIT_MAX_LEN};
    use crate::fs::ext4::dir::DirIter;
    use crate::pr_warn;

    pub struct Ext4SbInfo {
        pub ext4_sb: Ext4SuperBlock,
        pub ext4_gd: Ext4GroupDesc,
        pub inode_size: u32,
        pub block_size: usize,
        pub inodes_per_group: u32,
        pub blocks_per_group: u32,
        pub valid: bool,
    }

    const fn empty_ext4_info() -> Ext4SbInfo {
        Ext4SbInfo {
            ext4_sb: unsafe { core::mem::zeroed() },
            ext4_gd: Ext4GroupDesc {
                block_bitmap: 0, inode_bitmap: 0, inode_table: 0, free_blocks_count: 0,
                free_inodes_count: 0, used_dirs_count: 0, flags: 0, itable_unused: 0, checksum: 0,
            },
            inode_size: 128, block_size: 1024, inodes_per_group: 0, blocks_per_group: 0, valid: false,
        }
    }

    static mut EXT4_INFO: [Ext4SbInfo; NR_SUPER] = [const { empty_ext4_info() }; NR_SUPER];

    #[inline]
    pub fn ext4_info(n: usize) -> &'static Ext4SbInfo {
        unsafe { &*ptr::addr_of!(EXT4_INFO[n]) }
    }
    #[inline]
    pub fn ext4_info_mut(n: usize) -> &'static mut Ext4SbInfo {
        unsafe { &mut *ptr::addr_of_mut!(EXT4_INFO[n]) }
    }

    /// 将 VFS inode 写回磁盘。
    pub unsafe fn write_inode(ip: usize) {
        unsafe {
            let i = inode::inode(ip);
            let ino = i.i_ino;
            let sb_nr = i.i_sb;
            if sb_nr == NIL || ino == 0 { return; }
            let info = ext4_info(sb_nr);
            if !info.valid { return; }
            let dev = sb(sb_nr).s_dev;
            let inode_table = info.ext4_gd.inode_table;
            let byte_off = (ino as u64 - 1) * info.inode_size as u64;
            let block = inode_table + byte_off / info.block_size as u64;
            let block_off = (byte_off % info.block_size as u64) as usize;
            let b = match buffer::bread(dev, block as u32, info.block_size) {
                Some(b) => b, None => return,
            };
            let data = buffer::bh(b).data_mut();
            if block_off + 40 <= data.len() {
                // Write key fields back (mode, uid, size, nlink, flags, i_block)
                data[block_off..block_off+2].copy_from_slice(&i.i_mode.to_le_bytes());
                data[block_off+2..block_off+4].copy_from_slice(&i.i_uid.to_le_bytes());
                data[block_off+4..block_off+8].copy_from_slice(&(i.i_size as u32).to_le_bytes());
                data[block_off+26..block_off+28].copy_from_slice(&i.i_nlink.to_le_bytes());
                // Write i_block from data[] as direct block pointers (u16 LE)
                for k in 0..9 {
                    let off = block_off + 40 + k * 2;
                    if off + 2 <= data.len() {
                        data[off..off+2].copy_from_slice(&i.data[k].to_le_bytes());
                    }
                }
                // Also write indirect block pointers as zero
                for k in 9..15 {
                    let off = block_off + 40 + k * 4;
                    if off + 4 <= data.len() {
                        data[off..off+4].copy_from_slice(&0u32.to_le_bytes());
                    }
                }
                buffer::mark_buffer_dirty(b);
            }
            buffer::brelse(b);
        }
    }

    /// 从磁盘读取 ext4 inode 到 VFS inode。
    pub unsafe fn read_inode(ip: usize) {
        unsafe {
            let i = inode::inode(ip);
            let ino = i.i_ino;
            let sb_nr = i.i_sb;
            if sb_nr == NIL || ino == 0 { return; }
            let info = ext4_info(sb_nr);
            if !info.valid { return; }
            let dev = sb(sb_nr).s_dev;
            let inode_table = info.ext4_gd.inode_table;
            let byte_off = (ino as u64 - 1) * info.inode_size as u64;
            let block = inode_table + byte_off / info.block_size as u64;
            let block_off = (byte_off % info.block_size as u64) as usize;
            let b = match buffer::bread(dev, block as u32, info.block_size) {
                Some(b) => b, None => return,
            };
            let data = buffer::bh(b).data();
            if block_off + info.inode_size as usize <= data.len() {
                let raw = &data[block_off..block_off + info.inode_size as usize];
                if let Some(ei) = Ext4Inode::from_bytes(raw) {
                    i.i_mode = ei.i_mode;
                    i.i_uid = ei.uid() as u16;
                    i.i_gid = ei.gid() as u16;
                    i.i_nlink = ei.i_links_count as u16;
                    i.i_size = ei.i_size() as u32;
                    i.i_atime = ei.atime();
                    i.i_mtime = ei.mtime();
                    i.i_ctime = ei.ctime();
                    i.i_blksize = info.block_size as u32;
                    i.data = [0; 9];
                    if ei.uses_extent() {
                        let ib = ei.i_block_raw();
                        if u16::from_le_bytes([ib[0],ib[1]]) == 0xF30A && ib[2] > 0 {
                            let p = u32::from_le_bytes([ib[20],ib[21],ib[22],ib[23]]);
                            i.data[0] = p as u16;
                        }
                        i.i_flags |= super::super::inode_flags::EXT4_EXTENTS_FL as u64;
                    } else {
                        i.data[0] = ei.i_block[0] as u16;
                    }
                }
            }
            buffer::brelse(b);
        }
    }

    /// bmap: 逻辑块 → 物理块映射。
    /// 使用 `data[0..8]` 作为直接块指针（同 minix zones 模型）。
    pub unsafe fn bmap(ip: usize, block: u32, _create: bool) -> u32 {
        unsafe {
            let i = inode::inode(ip);
            if (block as usize) < 9 {
                let phys = core::ptr::read_volatile(&raw const i.data[block as usize]) as u32;
                if phys != 0 { return phys; }
            }
            0
        }
    }

    /// 分配块：将逻辑块号映射到物理块
    pub unsafe fn extend_inode_block(ip: usize, lblock: u32, phys: u32) {
        unsafe {
            let i = inode::inode(ip);
            if (lblock as usize) < 9 {
                core::ptr::write_volatile(&raw mut i.data[lblock as usize], phys as u16);
                i.i_dirt = true;
            }
        }
    }

    /// 释放文件的所有数据块
    pub unsafe fn truncate(ip: usize) {
        unsafe {
            let i = inode::inode(ip);
            let sb_nr = i.i_sb;
            if sb_nr == NIL { return; }
            let info = ext4_info(sb_nr);
            if !info.valid { return; }
            let gd = &mut ext4_info_mut(sb_nr).ext4_gd;
            let sb_data = &ext4_info(sb_nr).ext4_sb;
            for k in 0..9 {
                let phys = core::ptr::read_volatile(&raw const i.data[k]);
                if phys != 0 {
                    bitmap::free_block(sb_data, gd, 0, phys);
                    core::ptr::write_volatile(&raw mut i.data[k], 0);
                }
            }
            i.i_size = 0;
            i.i_dirt = true;
        }
    }

    /// ext4 文件读取
    pub unsafe fn ext4_file_read(ip: usize, pos: u64, buf: &mut [u8]) -> i64 {
        unsafe {
            let i = inode::inode(ip);
            let sb_nr = i.i_sb;
            if sb_nr == NIL { return -(errno::EIO as i64); }
            let info = ext4_info(sb_nr);
            let bs = info.block_size;
            let dev = sb(sb_nr).s_dev;
            let size = i.i_size as u64;
            if pos >= size { return 0; }
            let len = if pos + buf.len() as u64 > size { (size - pos) as usize } else { buf.len() };
            let mut read: usize = 0;
            let mut off = pos;
            while read < len {
                let block = (off / bs as u64) as u32;
                let block_off = (off % bs as u64) as usize;
                let phys = bmap(ip, block, false);
                if phys == 0 { break; }
                let b = match buffer::bread(dev, phys, bs) {
                    Some(b) => b, None => break,
                };
                let data = buffer::bh(b).data();
                let avail = (bs - block_off).min(len - read);
                ptr::copy_nonoverlapping(data[block_off..].as_ptr(), buf.as_mut_ptr().add(read), avail);
                read += avail; off += avail as u64;
                buffer::brelse(b);
            }
            read as i64
        }
    }

    /// ext4 文件写入
    pub unsafe fn ext4_file_write(ip: usize, pos: u64, buf: &[u8]) -> i64 {
        unsafe {
            let i = inode::inode(ip);
            let sb_nr = i.i_sb;
            if sb_nr == NIL { return -(errno::EIO as i64); }
            let info = ext4_info(sb_nr);
            if !info.valid { return -(errno::EIO as i64); }
            let bs = info.block_size;
            let dev = sb(sb_nr).s_dev;
            if i.is_rdonly() { return -(errno::EROFS as i64); }
            let end = pos + buf.len() as u64;
            let last_block = ((end + bs as u64 - 1) / bs as u64).saturating_sub(1) as u32;
            let current_blocks = i.i_size as u64 / bs as u64;
            if last_block as u64 >= current_blocks {
                let gd = &mut ext4_info_mut(sb_nr).ext4_gd;
                let sb_data = &ext4_info(sb_nr).ext4_sb;
                for blk in (current_blocks as u32)..=last_block {
                    if bmap(ip, blk, false) == 0 {
                        let phys = match bitmap::alloc_block(sb_data, gd, 0) {
                            Some(b) => b as u32,
                            None => {
                                let written = (blk as u64 * bs as u64).saturating_sub(pos) as usize;
                                if written == 0 || written > buf.len() { return -(errno::ENOSPC as i64); }
                                return write_to_blocks(ip, pos, &buf[..written], bs, dev);
                            }
                        };
                        extend_inode_block(ip, blk, phys);
                    }
                }
            }
            write_to_blocks(ip, pos, buf, bs, dev)
        }
    }

    unsafe fn write_to_blocks(ip: usize, pos: u64, buf: &[u8], bs: usize, dev: u16) -> i64 {
        let i = inode::inode(ip);
        let mut written: usize = 0;
        let mut off = pos;
        while written < buf.len() {
            let block = (off / bs as u64) as u32;
            let block_off = (off % bs as u64) as usize;
            let phys = bmap(ip, block, false);
            if phys == 0 { break; }
            if let Some(b) = buffer::bread(dev, phys, bs) {
                let data = bh(b).data_mut();
                let avail = (bs - block_off).min(buf.len() - written);
                data[block_off..block_off + avail].copy_from_slice(&buf[written..written + avail]);
                buffer::mark_buffer_dirty(b); buffer::brelse(b);
                written += avail; off += avail as u64;
            } else { break; }
        }
        let new_end = pos + written as u64;
        if new_end > i.i_size as u64 { i.i_size = new_end as u32; }
        i.i_dirt = true;
        written as i64
    }

    /// 更新 read_super 为完整版本（解析组描述符等）
    pub fn read_super_full(n: usize) -> bool {
        let dev = unsafe { sb(n).s_dev };
        if dev == 0 { return false; }
        let bn = match unsafe { buffer::bread(dev, 1, 1024) } {
            Some(b) => b, None => return false,
        };
        let data = unsafe { bh(bn).data() };
        if data.len() < 58 || data[56] != 0x53 || data[57] != 0xEF {
            unsafe { buffer::brelse(bn); } return false;
        }
        let block_size: usize = 1024 << (data[24] as usize);
        let bpg = u32::from_le_bytes([data[32], data[33], data[34], data[35]]);
        let inodes_pg = u32::from_le_bytes([data[40], data[41], data[42], data[43]]);
        let inode_size: u32 = {
            let rev = u32::from_le_bytes([data[76], data[77], data[78], data[79]]);
            if rev >= 1 {
                let sz = u16::from_le_bytes([data[88], data[89]]) as u32;
                if sz == 0 { 128 } else { sz }
            } else { 128 }
        };
        let features_incompat = u32::from_le_bytes([data[96], data[97], data[98], data[99]]);
        let desc_block = if block_size == 1024 { 2u32 } else { 1u32 };
        let desc_bn = match unsafe { buffer::bread(dev, desc_block, block_size) } {
            Some(b) => b, None => { unsafe { buffer::brelse(bn); } return false; }
        };
        let desc_data = unsafe { bh(desc_bn).data() };
        let desc_size = if (features_incompat & 0x0080) != 0 { 64 } else { 32 };
        let gd = match Ext4GroupDesc::from_bytes(&desc_data[..desc_size], desc_size) {
            Some(g) => g, None => { unsafe { buffer::brelse(desc_bn); buffer::brelse(bn); } return false; }
        };
        let ext4_sb = unsafe { Ext4SuperBlock::from_slice(data) };
        let info = ext4_info_mut(n);
        info.ext4_sb = ext4_sb; info.ext4_gd = gd; info.inode_size = inode_size;
        info.block_size = block_size; info.inodes_per_group = inodes_pg;
        info.blocks_per_group = bpg; info.valid = true;

        unsafe {
            let s = super_block::sb_ptr(n);
            (*s).s_magic = 0xEF53; (*s).s_blocksize = block_size as u32;
            (*s).s_blocksize_bits = (10 + (data[24] & 0x07)) as u8;
            (*s).s_dirsize = 32; (*s).s_namelen = 255;
            let ip = inode::iget(n, 2);
            if ip == NIL { unsafe { buffer::brelse(desc_bn); buffer::brelse(bn); } return false; }
            let i = inode::inode(ip);
            i.i_mode = 0o40755; i.i_nlink = 2; i.i_size = 1024;
            i.i_op = FsType::Ext2; i.i_sb = n;
            let info_ref = ext4_info(n);
            let inode_table = info_ref.ext4_gd.inode_table;
            let byte_off = 1u64 * inode_size as u64;
            let ino_block = inode_table + byte_off / block_size as u64;
            let ino_off = (byte_off % block_size as u64) as usize;
            if let Some(ino_bn) = buffer::bread(dev, ino_block as u32, block_size) {
                let idata = bh(ino_bn).data();
                if ino_off + inode_size as usize <= idata.len() {
                    if let Some(ei) = Ext4Inode::from_bytes(&idata[ino_off..ino_off + inode_size as usize]) {
                        i.i_uid = ei.uid() as u16; i.i_gid = ei.gid() as u16;
                        i.i_nlink = ei.i_links_count as u16; i.i_size = ei.i_size() as u32;
                        i.i_mode = ei.i_mode; i.i_atime = ei.atime(); i.i_mtime = ei.mtime(); i.i_ctime = ei.ctime();
                        let i_block = ei.i_block_raw();
                        // 从 extent 树中提取第一个数据块号存入 data[0]
                        if ei.uses_extent() {
                            let m = u16::from_le_bytes([i_block[0], i_block[1]]);
                            if m == 0xF30A && i_block[2] > 0 {
                                let ee_start_lo = u32::from_le_bytes([i_block[20], i_block[21], i_block[22], i_block[23]]);
                                i.data[0] = ee_start_lo as u16;
                                i.i_flags |= super::super::inode_flags::EXT4_EXTENTS_FL as u64;
                            }
                        } else {
                            i.data[0] = ei.i_block[0] as u16;
                        }
                    }
                }
                buffer::brelse(ino_bn);
            }
            (*s).s_mounted = ip; (*s).s_covered = NIL;
        }
        unsafe { buffer::brelse(desc_bn); buffer::brelse(bn); }
        true
    }
}
