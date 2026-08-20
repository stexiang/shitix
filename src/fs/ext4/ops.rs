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
        i.data = [0; 15];
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
                        i.data[0] = ee_start_lo as u32;
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
        pub ext4_gd: Ext4GroupDesc,  // group 0 descriptor
        pub inode_size: u32,
        pub block_size: usize,    // always 1024 for buffer cache
        pub fs_block_size: usize, // actual filesystem block size
        pub inodes_per_group: u32,
        pub blocks_per_group: u32,
        pub group_count: u32,     // total number of block groups
        pub gd_1024_block: u32,   // 1024-byte block where group descriptors start
        pub gd_size: usize,       // size of each group descriptor (32 or 64)
        pub valid: bool,
    }

    const fn empty_ext4_info() -> Ext4SbInfo {
        Ext4SbInfo {
            ext4_sb: unsafe { core::mem::zeroed() },
            ext4_gd: Ext4GroupDesc {
                block_bitmap: 0, inode_bitmap: 0, inode_table: 0, free_blocks_count: 0,
                free_inodes_count: 0, used_dirs_count: 0, flags: 0, itable_unused: 0, checksum: 0,
            },
            inode_size: 128, block_size: 1024, fs_block_size: 1024,
            inodes_per_group: 0, blocks_per_group: 0, group_count: 0,
            gd_1024_block: 0, gd_size: 32, valid: false,
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
            // 与 read_inode 对齐：用 inode_table_for_inode（已按 fs_block_size
            // 缩放到 1024 字节块号）并减 group_base，不能用未缩放的
            // info.ext4_gd.inode_table（那是 4096 字节文件系统块号），否则
            // inode 被写回错误块、破坏文件系统。
            let inode_table = match inode_table_for_inode(sb_nr, ino) {
                Some(t) => t as u64,
                None => return,
            };
            let group_base = ((ino as u64 - 1) / info.inodes_per_group as u64)
                * info.inodes_per_group as u64;
            let byte_off = (ino as u64 - 1 - group_base) * info.inode_size as u64;
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
                // dtime：删除时间。nlink==0 的 inode 记下删除时间，否则 e2fsck 报
                // "Deleted inode has zero dtime"。
                // 本内核没接 RTC，current_time() 会是一个很小的值（甚至 0），
                // 小值会被 e2fsck 当成孤儿链表里的 inode 号。用一个大到不可能
                // 是 inode 号的值当 dtime，既非 0（避免 "zero dtime"）又不至于
                // 被误读成孤儿指针。
                let dtime = if i.i_nlink == 0 { 0xFFFF_0000u32 } else { 0 };
                data[block_off+20..block_off+24].copy_from_slice(&dtime.to_le_bytes());
                data[block_off+24..block_off+26].copy_from_slice(&i.i_gid.to_le_bytes());
                data[block_off+26..block_off+28].copy_from_slice(&i.i_nlink.to_le_bytes());
                // i_blocks：分配的 512 字节扇区数（直接+间接指针块数 × fs_block_size/512）。
                let mut nblocks = 0u32;
                for k in 0..15 { if i.data[k] != 0 { nblocks += 1; } }
                let sectors = nblocks * (info.fs_block_size / 512) as u32;
                data[block_off+28..block_off+32].copy_from_slice(&sectors.to_le_bytes());
                // i_block 区：12 个直接块 + 3 个间接块指针，每个 u32 小端，
                // 单位是文件系统块（fs_block_size）。内存里的 i.data[0..15] 存的
                // 就是这 15 个指针（12 直接 + 3 间接），直接写回。
                for k in 0..15 {
                    let off = block_off + 40 + k * 4;
                    if off + 4 <= data.len() {
                        data[off..off+4].copy_from_slice(&i.data[k].to_le_bytes());
                    }
                }
                buffer::mark_buffer_dirty(b);
            }
            buffer::brelse(b);
        }
    }

    /// 写回 ext2/ext4 超级块与组描述符的空闲计数。对应原版 `ext2_write_super`。
    ///
    /// 本内核只维护组 0 的描述符（单块组文件系统），所以超级块空闲计数直接
    /// 取组 0 的计数。块位图/inode 位图的变更由 alloc/free 路径用 mark_buffer_dirty
    /// 落盘，这里只回写 free_blocks_count / free_inodes_count。
    pub unsafe fn write_super(n: usize) {
        unsafe {
            let info = ext4_info(n);
            if !info.valid { return; }
            let dev = sb(n).s_dev;
            let free_blocks = info.ext4_gd.free_blocks_count;
            let free_inodes = info.ext4_gd.free_inodes_count;

            // 超级块在 1024 字节块 1（字节偏移 1024）；s_free_blocks_count 在
            // 偏移 12、s_free_inodes_count 在偏移 16（u32 小端）。
            if let Some(sbn) = buffer::bread(dev, 1, 1024) {
                let sd = bh(sbn).data_mut();
                if sd.len() >= 20 {
                    sd[12..16].copy_from_slice(&free_blocks.to_le_bytes());
                    sd[16..20].copy_from_slice(&free_inodes.to_le_bytes());
                }
                buffer::mark_buffer_dirty(sbn);
                buffer::brelse(sbn);
            }

            // 组 0 描述符：bg_free_blocks_count 偏移 12（u16）、
            // bg_free_inodes_count 偏移 14（u16）、bg_used_dirs_count 偏移 16（u16）。
            let gd_block = info.gd_1024_block;
            let used_dirs = info.ext4_gd.used_dirs_count;
            if let Some(gbn) = buffer::bread(dev, gd_block, 1024) {
                let gd_data = bh(gbn).data_mut();
                if gd_data.len() >= 18 {
                    gd_data[12..14].copy_from_slice(&(free_blocks as u16).to_le_bytes());
                    gd_data[14..16].copy_from_slice(&(free_inodes as u16).to_le_bytes());
                    gd_data[16..18].copy_from_slice(&(used_dirs as u16).to_le_bytes());
                }
                buffer::mark_buffer_dirty(gbn);
                buffer::brelse(gbn);
            }
        }
    }

    /// 读任意块组的描述符（块号字段按 scale 缩放到 1024 字节块号）。
    /// 组 0 直接返回缓存的 ext4_gd；组 > 0 从 GDT 读。
    pub unsafe fn read_group_desc_scaled(sb_nr: usize, group: u32) -> Option<Ext4GroupDesc> {
        unsafe {
            let info = ext4_info(sb_nr);
            if !info.valid || group >= info.group_count { return None; }
            if group == 0 { return Some(info.ext4_gd); }
            let gd_byte_off = group as usize * info.gd_size;
            let gd_1024_off = info.gd_1024_block + (gd_byte_off / 1024) as u32;
            let gd_inblock_off = gd_byte_off % 1024;
            let dev = sb(sb_nr).s_dev;
            let bn = buffer::bread(dev, gd_1024_off, 1024)?;
            let data = bh(bn).data();
            if gd_inblock_off + info.gd_size > data.len() {
                buffer::brelse(bn);
                return None;
            }
            let gd_bytes = &data[gd_inblock_off..gd_inblock_off + info.gd_size];
            let mut gd = Ext4GroupDesc::from_bytes(gd_bytes, info.gd_size)?;
            let scale = (info.fs_block_size / 1024) as u64;
            gd.block_bitmap *= scale;
            gd.inode_bitmap *= scale;
            gd.inode_table *= scale;
            buffer::brelse(bn);
            Some(gd)
        }
    }

    /// 把组描述符的空闲计数写回 GDT（组 0 写缓存，组 > 0 写盘）。
    pub unsafe fn write_group_desc_scaled(sb_nr: usize, group: u32, gd: &Ext4GroupDesc) {
        unsafe {
            let info = ext4_info(sb_nr);
            if !info.valid { return; }
            if group == 0 {
                ext4_info_mut(sb_nr).ext4_gd.free_blocks_count = gd.free_blocks_count;
                ext4_info_mut(sb_nr).ext4_gd.free_inodes_count = gd.free_inodes_count;
                ext4_info_mut(sb_nr).ext4_gd.used_dirs_count = gd.used_dirs_count;
                return;
            }
            let gd_byte_off = group as usize * info.gd_size;
            let gd_1024_off = info.gd_1024_block + (gd_byte_off / 1024) as u32;
            let gd_inblock_off = gd_byte_off % 1024;
            let dev = sb(sb_nr).s_dev;
            if let Some(bn) = buffer::bread(dev, gd_1024_off, 1024) {
                let data = bh(bn).data_mut();
                if gd_inblock_off + 18 <= data.len() {
                    data[gd_inblock_off+12..gd_inblock_off+14].copy_from_slice(&(gd.free_blocks_count as u16).to_le_bytes());
                    data[gd_inblock_off+14..gd_inblock_off+16].copy_from_slice(&(gd.free_inodes_count as u16).to_le_bytes());
                    data[gd_inblock_off+16..gd_inblock_off+18].copy_from_slice(&(gd.used_dirs_count as u16).to_le_bytes());
                }
                buffer::mark_buffer_dirty(bn);
                buffer::brelse(bn);
            }
        }
    }

    /// 跨块组分配一个数据块（先组 0，再组 1..N）。返回全局文件系统块号。
    pub unsafe fn alloc_block_any(sb_nr: usize) -> Option<u64> {
        unsafe {
            let info = ext4_info(sb_nr);
            if !info.valid { return None; }
            let dev = sb(sb_nr).s_dev;
            let sb_data = &info.ext4_sb;
            let group_count = info.group_count as u32;
            for group in 0..group_count {
                if group == 0 {
                    let gd = &mut ext4_info_mut(sb_nr).ext4_gd;
                    if let Some(b) = bitmap::alloc_block(sb_data, gd, group, dev) {
                        return Some(b);
                    }
                } else {
                    let mut gd = read_group_desc_scaled(sb_nr, group)?;
                    if let Some(b) = bitmap::alloc_block(sb_data, &mut gd, group, dev) {
                        write_group_desc_scaled(sb_nr, group, &gd);
                        return Some(b);
                    }
                }
            }
            None
        }
    }

    /// 跨块组分配一个 inode。返回全局 inode 号。
    pub unsafe fn alloc_inode_any(sb_nr: usize) -> Option<u32> {
        unsafe {
            let info = ext4_info(sb_nr);
            if !info.valid { return None; }
            let dev = sb(sb_nr).s_dev;
            let sb_data = &info.ext4_sb;
            let group_count = info.group_count as u32;
            for group in 0..group_count {
                if group == 0 {
                    let gd = &mut ext4_info_mut(sb_nr).ext4_gd;
                    if let Some(ino) = bitmap::alloc_inode(sb_data, gd, group, dev) {
                        return Some(ino);
                    }
                } else {
                    let mut gd = read_group_desc_scaled(sb_nr, group)?;
                    if let Some(ino) = bitmap::alloc_inode(sb_data, &mut gd, group, dev) {
                        write_group_desc_scaled(sb_nr, group, &gd);
                        return Some(ino);
                    }
                }
            }
            None
        }
    }

    /// 跨块组释放一个数据块（根据块号定位组）。
    pub unsafe fn free_block_any(sb_nr: usize, block: u32) {
        unsafe {
            let info = ext4_info(sb_nr);
            if !info.valid { return; }
            let dev = sb(sb_nr).s_dev;
            let sb_data = &info.ext4_sb;
            let blocks_per_group = info.ext4_sb.s_blocks_per_group as u64;
            if blocks_per_group == 0 { return; }
            let group = (block as u64 / blocks_per_group) as u32;
            let block_offset = (block as u64 % blocks_per_group) as u32;
            if group == 0 {
                let gd = &mut ext4_info_mut(sb_nr).ext4_gd;
                bitmap::free_block(sb_data, gd, group, block_offset, dev);
            } else {
                let mut gd = read_group_desc_scaled(sb_nr, group).unwrap_or_default();
                bitmap::free_block(sb_data, &mut gd, group, block_offset, dev);
                write_group_desc_scaled(sb_nr, group, &gd);
            }
        }
    }

    /// 跨块组释放一个 inode（根据 inode 号定位组）。
    pub unsafe fn free_inode_any(sb_nr: usize, ino: u32) {
        unsafe {
            let info = ext4_info(sb_nr);
            if !info.valid || ino < 1 { return; }
            let dev = sb(sb_nr).s_dev;
            let sb_data = &info.ext4_sb;
            let inodes_per_group = info.ext4_sb.s_inodes_per_group as u64;
            if inodes_per_group == 0 { return; }
            let group = ((ino as u64 - 1) / inodes_per_group) as u32;
            let inode_offset = ((ino as u64 - 1) % inodes_per_group) as u32;
            if group == 0 {
                let gd = &mut ext4_info_mut(sb_nr).ext4_gd;
                bitmap::free_inode(sb_data, gd, group, inode_offset, dev);
            } else {
                let mut gd = read_group_desc_scaled(sb_nr, group).unwrap_or_default();
                bitmap::free_inode(sb_data, &mut gd, group, inode_offset, dev);
                write_group_desc_scaled(sb_nr, group, &gd);
            }
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
            let inode_table = match inode_table_for_inode(sb_nr, ino) {
                Some(t) => t as u64,
                None => return,
            };
            let group_base = ((ino as u64 - 1) / info.inodes_per_group as u64) * info.inodes_per_group as u64;
            let byte_off = (ino as u64 - 1 - group_base) * info.inode_size as u64;
            let block = inode_table + byte_off / info.block_size as u64;
            let block_off = (byte_off % info.block_size as u64) as usize;
            let b = match buffer::bread(dev, block as u32, info.block_size) {
                Some(b) => b, None => return,
            };
            let data = buffer::bh(b).data();
            if block_off + info.inode_size as usize <= data.len() {
                let raw = &data[block_off..block_off + info.inode_size as usize];
                if let Some(ei) = Ext4Inode::from_bytes(raw) {
                    i.i_op = FsType::Ext2;
                    i.i_mode = ei.i_mode;
                    i.i_uid = ei.uid() as u16;
                    i.i_gid = ei.gid() as u16;
                    i.i_nlink = ei.i_links_count as u16;
                    i.i_size = ei.i_size() as u32;
                    i.i_atime = ei.atime();
                    i.i_mtime = ei.mtime();
                    i.i_ctime = ei.ctime();
                    i.i_blksize = info.block_size as u32;
                    i.data = [0; 15];
                    if ei.uses_extent() {
                        let ib = ei.i_block_raw();
                        if u16::from_le_bytes([ib[0],ib[1]]) == 0xF30A {
                            let n_entries = ib[2] as usize;
                            // 按文件系统块粒度填充 i.data[k]：k 是文件系统逻辑块
                            // 号，值是文件系统物理块号（与 on-disk i_block 同单位）。
                            for e in 0..n_entries {
                                let off = 12 + e * 12; // header(12) + entry(12)
                                let ee_block = u32::from_le_bytes([ib[off],ib[off+1],ib[off+2],ib[off+3]]);
                                let ee_len = u16::from_le_bytes([ib[off+4],ib[off+5]]) as u32;
                                let ee_start_lo = u32::from_le_bytes([ib[off+8],ib[off+9],ib[off+10],ib[off+11]]);
                                for blk in 0..ee_len {
                                    let idx = (ee_block + blk) as usize;
                                    if idx < 12 {
                                        i.data[idx] = (ee_start_lo + blk) as u32;
                                    }
                                }
                            }
                        }
                        i.i_flags |= super::super::inode_flags::EXT4_EXTENTS_FL as u64;
                    } else {
                        // 经典（非 extent）文件/目录：把 12 个直接块 + 3 个间接
                        // 指针都读进 i.data[]。间接指针（单/双/三级）写路径
                        // extend_inode_block 也要维护，否则 >48KB 文件丢间接块。
                        for k in 0..12 {
                            i.data[k] = ei.i_block[k] as u32;
                        }
                        i.data[12] = ei.i_block_indirect;
                        i.data[13] = ei.i_block_double_indirect;
                        i.data[14] = ei.i_block_triple_indirect;
                    }
                }
            }
            buffer::brelse(b);
        }
    }

    /// 读符号链接的目标路径。
    ///
    /// ext4 把短链接（目标 ≤ 60 字节且 `i_blocks==0`）直接存在 inode 的
    /// `i_block` 区（快速符号链接）；长链接分配数据块，目标存在块里。
    /// 对应原版 `ext2_follow_link` 里 `inode->i_blocks ? bread : i_data`
    /// 的二分。返回目标的字节切片（不含 NUL），最长 255。
    ///
    /// # Safety
    /// 只能在进程上下文调用。`ip` 是已 `iget` 的符号链接 inode。
    pub unsafe fn read_symlink(ip: usize) -> Option<([u8; 256], usize)> {
        unsafe {
            let i = inode::inode(ip);
            let sb_nr = i.i_sb;
            if sb_nr == NIL { return None; }
            let info = ext4_info(sb_nr);
            if !info.valid { return None; }
            let dev = sb(sb_nr).s_dev;
            let ino = i.i_ino;
            let inode_table = inode_table_for_inode(sb_nr, ino)? as u64;
            let group_base = ((ino as u64 - 1) / info.inodes_per_group as u64)
                * info.inodes_per_group as u64;
            let byte_off = (ino as u64 - 1 - group_base) * info.inode_size as u64;
            let block = inode_table + byte_off / info.block_size as u64;
            let block_off = (byte_off % info.block_size as u64) as usize;
            let b = buffer::bread(dev, block as u32, info.block_size)?;
            let data = bh(b).data();
            if block_off + info.inode_size as usize > data.len() {
                buffer::brelse(b);
                return None;
            }
            let raw = &data[block_off..block_off + info.inode_size as usize];
            let ei = Ext4Inode::from_bytes(raw)?;
            let fast = ei.is_fast_symlink();
            // i_block 区起自偏移 40，长 60 字节。
            const IB_OFF: usize = 40;
            if fast {
                // 快速链接：目标内联在 i_block 前 i_size 字节
                let len = (ei.i_size() as usize).min(60).min(255);
                let mut o = [0u8; 256];
                o[..len].copy_from_slice(&raw[IB_OFF..IB_OFF + len]);
                buffer::brelse(b);
                return Some((o, len));
            }
            // 慢链接：bmap_phys（独立读盘，不依赖 b）求第一块物理号。先释放
            // inode 块缓冲再读数据块，避免同时占两块。
            let phys_1k = bmap_phys(ip, 0);
            buffer::brelse(b);
            if phys_1k == 0 {
                return None;
            }
            let scale = (info.block_size / 1024) as u32;
            let phys_fs = phys_1k / scale;
            let db = buffer::bread(dev, phys_fs, info.block_size)?;
            let d = bh(db).data();
            let mut len = 0;
            while len < d.len() && len < 255 && d[len] != 0 {
                len += 1;
            }
            let mut o = [0u8; 256];
            o[..len].copy_from_slice(&d[..len]);
            buffer::brelse(db);
            Some((o, len))
        }
    }

    /// bmap: 逻辑块 → 物理块映射。
    ///
    /// `block` 以 1024 字节为单位（与 `ext4_file_read` 的 `bs = block_size = 1024`
    /// 一致）。重新从磁盘读 inode，遍历 extent 树（或经典直接/间接块指针）
    /// 得到物理块号，避免 `i.data[0..9]` 只能记 9 个直接块的局限——大文件
    /// （如 /bin/sh）超出前 9 个逻辑块后旧实现返回 0，导致 execve 读到全零页。
    pub unsafe fn bmap(ip: usize, block: u32, _create: bool) -> u32 {
        unsafe {
            // 先查内存里的 i.data[]：以文件系统块为粒度（i.data[k] = 第 k 个
            // 文件系统逻辑块对应的物理文件系统块号）。写路径上 extend_inode_block
            // 只更新内存 i.data[] 还没来得及写盘，若直接 bmap_phys（读盘）会拿到
            // 旧的全零 i_block，导致刚分配的数据块写不进去。
            let i = inode::inode(ip);
            let sb_nr = i.i_sb;
            let info = ext4_info(sb_nr);
            let scale = if info.fs_block_size > 0 { (info.fs_block_size / 1024) as u32 } else { 1 };
            let lblock_fs = block / scale;
            let sub = block % scale;
            if (lblock_fs as usize) < 12 {
                let p = core::ptr::read_volatile(&raw const i.data[lblock_fs as usize]);
                if p != 0 { return (p as u32) * scale + sub; }
            }
            bmap_phys(ip, block)
        }
    }

    /// Read the inode table offset for the group containing `ino`.
    unsafe fn inode_table_for_inode(sb_nr: usize, ino: u32) -> Option<u32> {
        unsafe {
            let info = ext4_info(sb_nr);
            if !info.valid || info.inodes_per_group == 0 { return None; }
            let group = (ino as u64 - 1) / info.inodes_per_group as u64;
            if group == 0 {
                return Some(info.ext4_gd.inode_table as u32);
            }
            if group >= info.group_count as u64 {
                return None;
            }
            // Read the group descriptor from the group descriptor table
            let gd_byte_off = group as usize * info.gd_size;
            let gd_1024_off = info.gd_1024_block as u64 + (gd_byte_off / 1024) as u64;
            let gd_inblock_off = gd_byte_off % 1024;
            let dev = sb(sb_nr).s_dev;
            let bn = buffer::bread(dev, gd_1024_off as u32, 1024)?;
            let data = bh(bn).data();
            if gd_inblock_off + info.gd_size > data.len() {
                buffer::brelse(bn);
                return None;
            }
            let gd_bytes = &data[gd_inblock_off..gd_inblock_off + info.gd_size];
            // bg_inode_table is at offset 8 in the group descriptor (32-bit LE)
            let itable = u32::from_le_bytes([gd_bytes[8], gd_bytes[9], gd_bytes[10], gd_bytes[11]]);
            let scale = (info.fs_block_size / 1024) as u32;
            buffer::brelse(bn);
            Some(itable * scale)
        }
    }

    /// 读磁盘 inode 的原始 60 字节 `i_block` 区。失败返 `None`。
    unsafe fn read_inode_iblock(ip: usize) -> Option<([u8; 60], usize, u16)> {
        unsafe {
            let i = inode::inode(ip);
            let ino = i.i_ino;
            let sb_nr = i.i_sb;
            if sb_nr == NIL || ino == 0 { return None; }
            let info = ext4_info(sb_nr);
            if !info.valid { return None; }
            let dev = sb(sb_nr).s_dev;
            let inode_table = match inode_table_for_inode(sb_nr, ino) {
                Some(t) => t as u64,
                None => return None,
            };
            let group_base = ((ino as u64 - 1) / info.inodes_per_group as u64) * info.inodes_per_group as u64;
            let byte_off = (ino as u64 - 1 - group_base) * info.inode_size as u64;
            let blk = inode_table + byte_off / info.block_size as u64;
            let block_off = (byte_off % info.block_size as u64) as usize;
            let b = buffer::bread(dev, blk as u32, info.block_size)?;
            let data = bh(b).data();
            let need = block_off + info.inode_size as usize;
            if need > data.len() { buffer::brelse(b); return None; }
            let raw = &data[block_off..need];
            let ei = Ext4Inode::from_bytes(raw)?;
            // i_block 区起自偏移 40，长 60 字节。
            let off = 40;
            if off + 60 > raw.len() { buffer::brelse(b); return None; }
            let mut ib = [0u8; 60];
            ib.copy_from_slice(&raw[off..off + 60]);
            buffer::brelse(b);
            Some((ib, info.fs_block_size, dev))
        }
    }

    /// 在 extent 树里查逻辑块 `lblock`（fs 块单位）的物理块号（fs 块单位）。
    /// `ib` 是根节点（inode i_block 区 60 字节）。支持 depth 0（叶子）
    /// 与 depth>0（索引→下探磁盘块）。
    unsafe fn extent_lookup(ib: &[u8; 60], lblock: u32, dev: u16, fs_bs: usize) -> Option<u64> {
        unsafe {
            let root = ExtentNode::parse(ib)?;
            let lblock_fs = lblock;
            if root.header().is_leaf() {
                let e = root.lookup_leaf(lblock_fs)?;
                let phys = e.ee_start() + (lblock_fs as u64 - e.ee_block() as u64);
                return Some(phys);
            }
            // 索引节点：逐层下探，最多 5 层（ext4 树深上限）。
            let mut node_buf = *ib;
            for _ in 0..5 {
                let node = ExtentNode::parse(&node_buf[..])?;
                let child_blk = node.lookup_index(lblock_fs)?;
                let b = buffer::bread(dev, child_blk as u32, fs_bs)?;
                let data = bh(b).data();
                let take = core::cmp::min(data.len(), node_buf.len());
                node_buf[..take].copy_from_slice(&data[..take]);
                buffer::brelse(b);
                let child = ExtentNode::parse(&node_buf[..])?;
                if child.header().is_leaf() {
                    let e = child.lookup_leaf(lblock_fs)?;
                    let phys = e.ee_start() + (lblock_fs as u64 - e.ee_block() as u64);
                    return Some(phys);
                }
            }
            None
        }
    }

    /// 经典（非 extent）块映射：12 直接 + 1 一级 + 1 二级 + 1 三级间接。
    /// `lblock`/返回值均以 fs 块为单位。指针在 i_block 区：[0..12] 直接，
    /// [12] 一级，[13] 二级，[14] 三级（每指针 4 字节小端）。
    unsafe fn classic_lookup(ib: &[u8; 60], lblock: u32, dev: u16, fs_bs: usize) -> Option<u32> {
        unsafe {
            let ptrs_per_block = (fs_bs / 4) as u32;
            let rd32 = |o: usize| -> u32 {
                u32::from_le_bytes([ib[o], ib[o + 1], ib[o + 2], ib[o + 3]])
            };
            // 12 直接块（偏移 0..48）
            if lblock < 12 {
                let p = rd32((lblock as usize) * 4);
                return if p != 0 { Some(p) } else { None };
            }
            let mut idx = lblock - 12;
            // 一级间接：偏移 48
            if idx < ptrs_per_block {
                let ind = rd32(48);
                if ind == 0 { return None; }
                return read_indirect(dev, ind, idx, fs_bs);
            }
            idx -= ptrs_per_block;
            // 二级间接：偏移 52
            if idx < ptrs_per_block * ptrs_per_block {
                let dind = rd32(52);
                if dind == 0 { return None; }
                let outer = idx / ptrs_per_block;
                let inner = idx % ptrs_per_block;
                // Read outer pointer from double-indirect block using 1024-byte buffer
                let scale = (fs_bs / 1024) as u32;
                let dptr_off = (outer as usize) * 4;
                let dblock_1024 = dind * scale + (dptr_off / 1024) as u32;
                let b = buffer::bread(dev, dblock_1024, 1024)?;
                let data = bh(b).data();
                let off = dptr_off % 1024;
                let ind = if off + 4 <= data.len() {
                    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                } else { 0 };
                buffer::brelse(b);
                if ind == 0 { return None; }
                return read_indirect(dev, ind, inner, fs_bs);
            }
            idx -= ptrs_per_block * ptrs_per_block;
            // 三级间接：偏移 56
            let tind = rd32(56);
            if tind == 0 { return None; }
            let ppb2 = ptrs_per_block * ptrs_per_block;
            let outer = idx / ppb2;
            let mid = (idx % ppb2) / ptrs_per_block;
            let inner = idx % ptrs_per_block;
            let scale = (fs_bs / 1024) as u32;
            // Read triple-indirect pointer using 1024-byte buffer
            let tptr_off = (outer as usize) * 4;
            let tblock_1024 = tind * scale + (tptr_off / 1024) as u32;
            let b = buffer::bread(dev, tblock_1024, 1024)?;
            let data = bh(b).data();
            let off = tptr_off % 1024;
            let dind = if off + 4 <= data.len() {
                u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
            } else { 0 };
            buffer::brelse(b);
            if dind == 0 { return None; }
            // Read double-indirect pointer using 1024-byte buffer
            let dptr_off2 = (mid as usize) * 4;
            let dblock_1024_2 = dind * scale + (dptr_off2 / 1024) as u32;
            let b2 = buffer::bread(dev, dblock_1024_2, 1024)?;
            let data2 = bh(b2).data();
            let off2 = dptr_off2 % 1024;
            let ind = if off2 + 4 <= data2.len() {
                u32::from_le_bytes([data2[off2], data2[off2 + 1], data2[off2 + 2], data2[off2 + 3]])
            } else { 0 };
            buffer::brelse(b2);
            if ind == 0 { return None; }
            read_indirect(dev, ind, inner, fs_bs)
        }
    }

    /// 读一级间接块 `ind` 中第 `idx` 个指针。
    /// `ind` is in filesystem blocks (e.g. 4096 bytes). Since our buffer cache
    /// only has 1024-byte buffers, we scale `ind` to 1024-byte units and read
    /// the needed sub-block.
    unsafe fn read_indirect(dev: u16, ind: u32, idx: u32, fs_bs: usize) -> Option<u32> {
        unsafe {
            let scale = (fs_bs / 1024) as u32;
            let ptr_offset = (idx as usize) * 4;
            let block_1024 = ind * scale + (ptr_offset / 1024) as u32;
            let b = buffer::bread(dev, block_1024, 1024)?;
            let data = bh(b).data();
            let off = ptr_offset % 1024;
            let p = if off + 4 <= data.len() {
                u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
            } else { 0 };
            buffer::brelse(b);
            if p != 0 { Some(p) } else { None }
        }
    }

    /// bmap 的实现：返回 1024 字节单位的物理块号。
    unsafe fn bmap_phys(ip: usize, block: u32) -> u32 {
        unsafe {
            let (ib_disk, fs_bs, dev) = match read_inode_iblock(ip) {
                Some(v) => v,
                None => return 0,
            };
            let scale = (fs_bs / 1024) as u32;
            // block 是 1024 字节单位；extent/classic 用 fs 块单位。
            let lblock_fs = block / scale;
            let sub = block % scale; // 块内 1024 子块偏移
            // i_block 区头两个字节为 extent 树根 magic（0xF30A）即用 extent。
            let uses_ext = u16::from_le_bytes([ib_disk[0], ib_disk[1]]) == 0xF30A;
            if uses_ext {
                if let Some(phys_fs) = extent_lookup(&ib_disk, lblock_fs, dev, fs_bs) {
                    return (phys_fs as u32) * scale + sub;
                }
                return 0;
            }
            // 经典直接/间接块：用内存里的 i.data[0..15]（12 直接 + 3 间接指针），
            // 而不是磁盘 i_block。写路径 extend_inode_block 更新的是内存 i.data，
            // 磁盘要等 write_inode 才同步——若读磁盘会拿到旧的 0 间接指针，导致
            // 间接块永远映射不上（bug-066）。
            let i = inode::inode(ip);
            let mut ib = [0u8; 60];
            for k in 0..15 {
                ib[k * 4..k * 4 + 4].copy_from_slice(&i.data[k].to_le_bytes());
            }
            if let Some(phys_fs) = classic_lookup(&ib, lblock_fs, dev, fs_bs) {
                return phys_fs * scale + sub;
            }
            0
        }
    }

    /// 写一级间接块 `ind` 的第 `idx` 个指针为 `phys`。
    /// `ind` 是文件系统块号，`scale = fs_bs/1024`，缓冲按 1024 字节读。
    unsafe fn set_indirect(dev: u16, ind: u32, idx: u32, phys: u32, scale: u32) {
        unsafe {
            let ptr_off = (idx as usize) * 4;
            let block_1024 = ind * scale + (ptr_off / 1024) as u32;
            if let Some(b) = buffer::bread(dev, block_1024, 1024) {
                let data = buffer::bh(b).data_mut();
                let off = ptr_off % 1024;
                if off + 4 <= data.len() {
                    data[off..off + 4].copy_from_slice(&phys.to_le_bytes());
                }
                buffer::mark_buffer_dirty(b);
                buffer::brelse(b);
            }
        }
    }

    /// 读一级间接块 `ind` 的第 `idx` 个指针。
    unsafe fn get_indirect(dev: u16, ind: u32, idx: u32, scale: u32) -> u32 {
        unsafe {
            let ptr_off = (idx as usize) * 4;
            let block_1024 = ind * scale + (ptr_off / 1024) as u32;
            if let Some(b) = buffer::bread(dev, block_1024, 1024) {
                let data = buffer::bh(b).data();
                let off = ptr_off % 1024;
                let p = if off + 4 <= data.len() {
                    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                } else {
                    0
                };
                buffer::brelse(b);
                p
            } else {
                0
            }
        }
    }

    /// 分配块：将逻辑块号（fs 块单位）映射到物理块号（fs 块单位）。
    /// 维护 i.data[0..15]（12 直接 + 3 间接指针）；间接数据块指针写进
    /// 一级/二级/三级间接块（bug-066 之前只写直接块，>48KB 文件丢块）。
    pub unsafe fn extend_inode_block(ip: usize, lblock: u32, phys: u32) {
        unsafe {
            let i = inode::inode(ip);
            let sb_nr = i.i_sb;
            if sb_nr == NIL { return; }
            let info = ext4_info(sb_nr);
            if !info.valid { return; }
            let dev = sb(sb_nr).s_dev;
            let fs_bs = info.fs_block_size as u32;
            let ptrs = (fs_bs / 4) as u32; // 每块可装多少个 u32 指针
            let scale = (fs_bs / 1024) as u32;

            if (lblock as usize) < 12 {
                core::ptr::write_volatile(&raw mut i.data[lblock as usize], phys);
                i.i_dirt = true;
                return;
            }

            // 一级间接：i.data[12]
            let mut idx = lblock - 12;
            if idx < ptrs {
                if i.data[12] == 0 {
                    let ind = match alloc_block_any(sb_nr) {
                        Some(b) => b as u32,
                        None => return,
                    };
                    core::ptr::write_volatile(&raw mut i.data[12], ind);
                    i.i_dirt = true;
                }
                set_indirect(dev, i.data[12], idx, phys, scale);
                return;
            }

            // 二级间接：i.data[13]
            idx -= ptrs;
            if idx < ptrs * ptrs {
                if i.data[13] == 0 {
                    let dind = match alloc_block_any(sb_nr) {
                        Some(b) => b as u32,
                        None => return,
                    };
                    core::ptr::write_volatile(&raw mut i.data[13], dind);
                    i.i_dirt = true;
                }
                let outer = idx / ptrs;
                let inner = idx % ptrs;
                let mut ind = get_indirect(dev, i.data[13], outer, scale);
                if ind == 0 {
                    ind = match alloc_block_any(sb_nr) {
                        Some(b) => b as u32,
                        None => return,
                    };
                    set_indirect(dev, i.data[13], outer, ind, scale);
                }
                set_indirect(dev, ind, inner, phys, scale);
                return;
            }

            // 三级间接：i.data[14]
            idx -= ptrs * ptrs;
            if idx < ptrs * ptrs * ptrs {
                if i.data[14] == 0 {
                    let tind = match alloc_block_any(sb_nr) {
                        Some(b) => b as u32,
                        None => return,
                    };
                    core::ptr::write_volatile(&raw mut i.data[14], tind);
                    i.i_dirt = true;
                }
                let outer = idx / (ptrs * ptrs);
                let mid = (idx % (ptrs * ptrs)) / ptrs;
                let inner = idx % ptrs;
                let mut dind = get_indirect(dev, i.data[14], outer, scale);
                if dind == 0 {
                    dind = match alloc_block_any(sb_nr) {
                        Some(b) => b as u32,
                        None => return,
                    };
                    set_indirect(dev, i.data[14], outer, dind, scale);
                }
                let mut ind = get_indirect(dev, dind, mid, scale);
                if ind == 0 {
                    ind = match alloc_block_any(sb_nr) {
                        Some(b) => b as u32,
                        None => return,
                    };
                    set_indirect(dev, dind, mid, ind, scale);
                }
                set_indirect(dev, ind, inner, phys, scale);
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
            let dev = sb(sb_nr).s_dev;
            let gd = &mut ext4_info_mut(sb_nr).ext4_gd;
            let sb_data = &ext4_info(sb_nr).ext4_sb;
            for k in 0..15 {
                let phys = core::ptr::read_volatile(&raw const i.data[k]);
                if phys != 0 {
                    free_block_any(sb_nr, phys);
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
            // i.data[] / extend_inode_block 以文件系统块为粒度。
            let scale = if info.fs_block_size > 0 { (info.fs_block_size / 1024) as u32 } else { 1 };
            let end = pos + buf.len() as u64;
            let last_block = ((end + bs as u64 - 1) / bs as u64).saturating_sub(1) as u32;
            let current_blocks = i.i_size as u64 / bs as u64;
            if last_block as u64 >= current_blocks {
                let gd = &mut ext4_info_mut(sb_nr).ext4_gd;
                let sb_data = &ext4_info(sb_nr).ext4_sb;
                for blk in (current_blocks as u32)..=last_block {
                    if bmap(ip, blk, false) == 0 {
                        let phys = match alloc_block_any(sb_nr) {
                            Some(b) => b as u32,
                            None => {
                                let written = (blk as u64 * bs as u64).saturating_sub(pos) as usize;
                                if written == 0 || written > buf.len() { return -(errno::ENOSPC as i64); }
                                return write_to_blocks(ip, pos, &buf[..written], bs, dev);
                            }
                        };
                        // phys 是文件系统块号，blk 是 1024 字节逻辑块号 → 换回 fs 索引。
                        extend_inode_block(ip, blk / scale, phys);
                    }
                }
            }
            let written = write_to_blocks(ip, pos, buf, bs, dev);
            // 分配了新块/改了位图与空闲计数后立刻落盘。本内核没有 bdflush
            // 守护进程，不主动 sync 的话这些脏数据会滞留到下次显式 sync。
            if written > 0 {
                buffer::sync_dev(dev);
            }
            written
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
        let fs_block_size: usize = 1024 << (data[24] as usize);
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
        // Always use 1024-byte buffer reads to avoid cache key conflicts.
        // For filesystems with 4096-byte blocks, the descriptor table is at
        // 1024-byte block numbers: 2 (for 1024B blocks) or 4 (for 4096B blocks).
        let desc_1024_block = if fs_block_size == 1024 { 2u32 } else { 4u32 };
        let desc_bn = match unsafe { buffer::bread(dev, desc_1024_block, 1024) } {
            Some(b) => b, None => { unsafe { buffer::brelse(bn); } return false; }
        };
        let desc_data = unsafe { bh(desc_bn).data() };
        let desc_size = if (features_incompat & 0x0080) != 0 { 64 } else { 32 };
        let mut gd = match Ext4GroupDesc::from_bytes(&desc_data[..desc_size], desc_size) {
            Some(g) => g, None => { unsafe { buffer::brelse(desc_bn); buffer::brelse(bn); } return false; }
        };
        let ext4_sb = unsafe { Ext4SuperBlock::from_slice(data) };
        let info = ext4_info_mut(n);
        let scale = (fs_block_size / 1024) as u32; // 1 for 1K, 4 for 4K blocks
        // Scale filesystem-block fields to 1024-byte buffer blocks
        let mut gd = gd;
        gd.block_bitmap *= scale as u64;
        gd.inode_bitmap *= scale as u64;
        gd.inode_table *= scale as u64;
        // 注意：free_blocks_count/free_inodes_count 是「计数」不是块号，
        // 不能乘 scale（alloc_block 每分一个文件系统块只减 1）。
        info.ext4_sb = ext4_sb; info.ext4_gd = gd; info.inode_size = inode_size;
        info.block_size = 1024; // Always use 1024-byte buffer blocks
        info.fs_block_size = fs_block_size; // Actual filesystem block size for scaling
        info.inodes_per_group = inodes_pg;
        info.blocks_per_group = bpg * scale;
        info.gd_1024_block = desc_1024_block;
        info.gd_size = desc_size;
        // Compute group count: total inodes / inodes_per_group (ceiling)
        let total_inodes = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        info.group_count = if inodes_pg > 0 { (total_inodes + inodes_pg - 1) / inodes_pg } else { 0 };
        info.valid = true;

        unsafe {
            let s = super_block::sb_ptr(n);
            // Always use 1024-byte blocks for buffer cache compatibility.
            // Filesystem with 4096-byte blocks multiplies block numbers by 4.
            (*s).s_magic = 0xEF53; (*s).s_blocksize = 1024;
            (*s).s_blocksize_bits = 10;
            (*s).s_dirsize = 32; (*s).s_namelen = 255;
            let ip = inode::iget(n, 2);
            if ip == NIL { unsafe { buffer::brelse(desc_bn); buffer::brelse(bn); } return false; }
            let i = inode::inode(ip);
            i.i_op = FsType::Ext2; i.i_sb = n;
            let info_ref = ext4_info(n);
            let inode_table = info_ref.ext4_gd.inode_table;
            let byte_off = 1u64 * inode_size as u64;
            let ino_block = inode_table + byte_off / info.block_size as u64;
            let ino_off = (byte_off % info.block_size as u64) as usize;
            if let Some(ino_bn) = buffer::bread(dev, ino_block as u32, info.block_size) {
                let idata = bh(ino_bn).data();
                if ino_off + inode_size as usize <= idata.len() {
                    if let Some(ei) = Ext4Inode::from_bytes(&idata[ino_off..ino_off + inode_size as usize]) {
                        i.i_uid = ei.uid() as u16; i.i_gid = ei.gid() as u16;
                        i.i_nlink = ei.i_links_count as u16; i.i_size = ei.i_size() as u32;
                        i.i_mode = ei.i_mode; i.i_atime = ei.atime(); i.i_mtime = ei.mtime(); i.i_ctime = ei.ctime();
                        let i_block = ei.i_block_raw();
                        // 按文件系统块粒度填充 i.data[k]（与 read_inode 同单位）。
                        if ei.uses_extent() {
                            let m = u16::from_le_bytes([i_block[0], i_block[1]]);
                            if m == 0xF30A {
                                let n_entries = i_block[2] as usize;
                                for e in 0..n_entries {
                                    let off = 12 + e * 12;
                                    let ee_blk = u32::from_le_bytes([i_block[off],i_block[off+1],i_block[off+2],i_block[off+3]]);
                                    let ee_len = u16::from_le_bytes([i_block[off+4],i_block[off+5]]) as u32;
                                    let ee_lo = u32::from_le_bytes([i_block[off+8],i_block[off+9],i_block[off+10],i_block[off+11]]);
                                    for blk in 0..ee_len {
                                        let idx = (ee_blk + blk) as usize;
                                        if idx < 12 { i.data[idx] = (ee_lo + blk) as u32; }
                                    }
                                }
                                i.i_flags |= super::super::inode_flags::EXT4_EXTENTS_FL as u64;
                            }
                        } else {
                            for k in 0..12 {
                                i.data[k] = ei.i_block[k] as u32;
                            }
                            i.data[12] = ei.i_block_indirect;
                            i.data[13] = ei.i_block_double_indirect;
                            i.data[14] = ei.i_block_triple_indirect;
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
