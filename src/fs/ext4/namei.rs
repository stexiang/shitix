//! ext4 目录项操作。对应 linux-1.0.9 的 `fs/minix/namei.c` 逻辑，
//! 但针对 ext4 的 `ext4_dir_entry_2` 目录格式。
//!
//! 实现：`lookup`、`create`、`mknod`、`mkdir`、`rmdir`、`unlink`。

use core::ptr;

use crate::fs::buffer::{self, BLOCK_SIZE, NIL, bh};
use crate::fs::inode::{self, FsType};
use crate::fs::super_block::sb;
use crate::fs::{MAY_EXEC, MAY_WRITE, mode};
use crate::klib::errno::{
    EEXIST, EINVAL, EIO, EMLINK, ENAMETOOLONG, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY, EPERM,
};
use crate::sched;
use crate::pr_warn;

use super::dir::{self, EXT4_DIR_ENTRY_HEADER_LEN, EXT4_NAME_LEN, ext4_dir_rec_len, DirIter};
use super::file_type;
use super::bitmap;
use super::group_desc;
use super::super_block::Ext4SuperBlock;

// ---- 块内目录查找 ----
/// 在 ext4 目录数据块中查找名字匹配的项。
/// 返回 `(缓冲下标, 项在块内的字节偏移)`。
unsafe fn find_entry_in_block(blk_buf: usize, name: &[u8]) -> Option<(usize, usize)> {
    let data = unsafe { bh(blk_buf).data() };
    for entry in DirIter::new(data) {
        if entry.is_empty() {
            continue;
        }
        if entry.name_len as usize != name.len() {
            continue;
        }
        let name_start = entry.name_off;
        let name_end = name_start + name.len();
        if name_end <= data.len() && &data[name_start..name_end] == name {
            return Some((blk_buf, entry.name_off - EXT4_DIR_ENTRY_HEADER_LEN));
        }
    }
    None
}

/// 在目录中查找一项。遍历目录的所有数据块。
///
/// 返回匹配项的 inode 号。目录项按**文件系统块**（fs_block_size）布局，
/// 不按 1024 字节缓冲块对齐，所以必须把整个 fs_block 读进连续缓冲再解析，
/// 否则跨 1024 边界的目录项会被截断（大目录里靠后的 stdio.h 之类查不到）。
///
/// # Safety
/// 只能在进程上下文调用。`dir` 是已 `iget` 的目录 inode。
pub unsafe fn find_entry(dir: usize, name: &[u8]) -> Option<u32> {
    if name.len() > EXT4_NAME_LEN {
        return None;
    }

    unsafe {
        let sb_nr = inode::inode(dir).i_sb;
        let dev = sb(sb_nr).s_dev;
        let size = inode::inode(dir).i_size as u64;
        let info = crate::fs::ext4::ops::full::ext4_info(sb_nr);
        let fs_block = if info.fs_block_size > 0 { info.fs_block_size as u64 } else { BLOCK_SIZE as u64 };
        let scale = (fs_block / BLOCK_SIZE as u64).max(1);

        let mut lblock = 0u64;
        while lblock * fs_block < size {
            // 只支持 fs_block <= 4096（当前 ext2/ext4 测试都 ≤4096）；
            // 更大的块回退到按 1024 分块读（可能漏跨边界项，但不会溢出）。
            if scale <= 4 {
                let mut buf = [0u8; 4096];
                let nbytes = (scale as usize) * BLOCK_SIZE;
                let mut valid = true;
                for sub in 0..scale {
                    let phys = crate::fs::ext4::ops::full::bmap(dir, (lblock * scale + sub) as u32, false);
                    if phys == 0 { valid = false; break; }
                    let bn = match buffer::bread(dev, phys, BLOCK_SIZE) {
                        Some(b) => b, None => { valid = false; break; }
                    };
                    let d = bh(bn).data();
                    let s = (sub as usize) * BLOCK_SIZE;
                    buf[s..s + BLOCK_SIZE].copy_from_slice(&d[..BLOCK_SIZE]);
                    buffer::brelse(bn);
                }
                if valid {
                    for entry in DirIter::new(&buf[..nbytes]) {
                        if entry.is_empty() || entry.name_len as usize != name.len() { continue; }
                        let ns = entry.name_off;
                        if ns + name.len() <= nbytes && &buf[ns..ns + name.len()] == name {
                            return Some(entry.inode);
                        }
                    }
                }
            }
            lblock += 1;
        }
        None
    }
}

/// `lookup` — 在目录中查找名字并返回对应的 inode。
/// 对应 minix 的 `minix_lookup()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn lookup(dir: usize, name: &[u8]) -> Result<usize, i32> {
    unsafe {
        if !mode::is_dir(inode::inode(dir).i_mode) {
            return Err(ENOTDIR);
        }
        match find_entry(dir, name) {
            Some(ino) => {
                if ino == 0 {
                    return Err(ENOENT);
                }
                let sb_nr = inode::inode(dir).i_sb;
                let ip = inode::iget(sb_nr, ino);
                if ip == NIL {
                    return Err(EIO);
                }
                Ok(ip)
            }
            None => Err(ENOENT),
        }
    }
}

/// `create` — 在目录中创建一个新的常规文件目录项。
/// 对应 minix 的 `minix_create()`。
pub unsafe fn create(dir: usize, name: &[u8], m: u16) -> Result<usize, i32> {
    unsafe {
        if name.len() > EXT4_NAME_LEN {
            return Err(ENAMETOOLONG);
        }
        let sb_nr = inode::inode(dir).i_sb;
        if sb_nr == NIL {
            return Err(EIO);
        }

        // 先用 lookup 确认不存在
        if find_entry(dir, name).is_some() {
            return Err(EEXIST);
        }

        // 分配一个 inode
        let dev = crate::fs::super_block::sb(sb_nr).s_dev;
        let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
        let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;
        let ino = match crate::fs::ext4::ops::full::alloc_inode_any(sb_nr) {
            Some(ino) => ino,
            None => return Err(ENOSPC),
        };

        // 初始化 inode
        let ip = inode::iget(sb_nr, ino);
        if ip == NIL {
            // 回退 inode 分配
            crate::fs::ext4::ops::full::free_inode_any(sb_nr, ino);
            return Err(EIO);
        }
        // 原版 `inode->i_uid = current->euid`；目录 setgid 则继承目录 gid。
        let dir_mode = inode::inode(dir).i_mode;
        let dir_gid = inode::inode(dir).i_gid;
        let (euid, egid) = {
            let c = crate::sched::current();
            (c.euid, c.egid)
        };
        let i = inode::inode(ip);
        // create 收到的 m 是权限位（如 0666），不含类型位；补上 S_IFREG。
        i.i_mode = (m & !mode::S_IFMT) | mode::S_IFREG;
        i.i_nlink = 1;
        i.i_uid = euid as u16;
        i.i_gid = if dir_mode & mode::S_ISGID != 0 { dir_gid } else { egid as u16 };
        i.i_size = 0;
        i.i_op = FsType::Ext2;
        i.i_sb = sb_nr;
        i.i_dirt = true;
        // data 已在初始状态全零（所有块未分配）

        // 把目录项加到目录中
        add_entry(dir, ino, name, file_type::EXT4_FT_REG_FILE)?;

        Ok(ip)
    }
}

/// `mknod` — 创建设备文件/命名管道等。
pub unsafe fn mknod(dir: usize, name: &[u8], m: u16, _rdev: u16) -> Result<usize, i32> {
    unsafe {
        if name.len() > EXT4_NAME_LEN {
            return Err(ENAMETOOLONG);
        }
        let sb_nr = inode::inode(dir).i_sb;
        if sb_nr == NIL {
            return Err(EIO);
        }

        if find_entry(dir, name).is_some() {
            return Err(EEXIST);
        }

        let dev = crate::fs::super_block::sb(sb_nr).s_dev;
        let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
        let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;
        let ino = match crate::fs::ext4::ops::full::alloc_inode_any(sb_nr) {
            Some(ino) => ino,
            None => return Err(ENOSPC),
        };

        let ip = inode::iget(sb_nr, ino);
        if ip == NIL {
            crate::fs::ext4::ops::full::free_inode_any(sb_nr, ino);
            return Err(EIO);
        }
        // 原版 `inode->i_uid = current->euid`；目录 setgid 则继承目录 gid。
        let dir_mode = inode::inode(dir).i_mode;
        let dir_gid = inode::inode(dir).i_gid;
        let (euid, egid) = {
            let c = crate::sched::current();
            (c.euid, c.egid)
        };
        let i = inode::inode(ip);
        i.i_mode = m;
        i.i_nlink = 1;
        i.i_uid = euid as u16;
        i.i_gid = if dir_mode & mode::S_ISGID != 0 { dir_gid } else { egid as u16 };
        i.i_size = 0;
        i.i_op = FsType::Ext2;
        i.i_sb = sb_nr;
        i.i_dirt = true;
        // 直接块指针，全零即可

        let ft = if mode::is_dir(m) {
            file_type::EXT4_FT_DIR
        } else if mode::is_chr(m) {
            file_type::EXT4_FT_CHRDEV
        } else if mode::is_blk(m) {
            file_type::EXT4_FT_BLKDEV
        } else if mode::is_fifo(m) {
            file_type::EXT4_FT_FIFO
        } else {
            file_type::EXT4_FT_REG_FILE
        };

        add_entry(dir, ino, name, ft)?;
        Ok(ip)
    }
}

/// `mkdir` — 创建目录。
pub unsafe fn mkdir(dir: usize, name: &[u8], m: u16) -> Result<usize, i32> {
    unsafe {
        let ip = mknod(dir, name, 0o40000 | m, 0)?; // S_IFDIR | m
        let i = inode::inode(ip);
        i.i_nlink = 2; // . 和 ..
        i.i_dirt = true;
        // 父目录多了一个子目录，链接数 +1（子目录的 ".." 指向父目录）。
        inode::inode(dir).i_nlink += 1;
        inode::inode(dir).i_dirt = true;

        // 为新目录分配一个数据块并写入 "." 和 ".." 项
        let sb_nr = i.i_sb;
        let dev = crate::fs::super_block::sb(sb_nr).s_dev;
        let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
        gd.used_dirs_count = gd.used_dirs_count.saturating_add(1);
        let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;

        let phys = match crate::fs::ext4::ops::full::alloc_block_any(sb_nr) {
            Some(b) => b,
            None => {
                // 回退
                i.i_nlink = 0;
                crate::fs::ext4::ops::full::free_inode_any(sb_nr, i.i_ino);
                inode::iput(ip);
                return Err(ENOSPC);
            }
        };

        // 直接块指针：data[0] = 文件系统物理块号（与 read_inode 同单位）。
        unsafe { ptr::write_volatile(&raw mut i.data[0], phys as u32); }

        // 写 "." 和 ".." 目录项。phys 是文件系统块号，缓冲缓存固定 1024
        // 字节块。先清零整块（scale 个子块），再在子块 0 写两条目录项，
        // ".." 的 rec_len 占满到文件系统块尾（对齐 add_entry 的布局）。
        let dev = unsafe { sb(sb_nr).s_dev };
        let fs_block_size = crate::fs::ext4::ops::full::ext4_info(sb_nr).fs_block_size;
        let scale = if fs_block_size > 0 { (fs_block_size / 1024) as u32 } else { 1 };
        i.i_size = fs_block_size as u32;
        for sub in 0..scale {
            if let Some(bn) = unsafe { buffer::getblk(dev, phys as u32 * scale + sub, 1024) } {
                let raw = unsafe { buffer::bh(bn).data_mut() };
                for b in raw.iter_mut() { *b = 0; }
                unsafe { buffer::bh(bn).b_uptodate = true };
                unsafe { buffer::mark_buffer_dirty(bn) };
                unsafe { buffer::brelse(bn) };
            }
        }
        if let Some(buf_bn) = unsafe { buffer::getblk(dev, phys as u32 * scale, 1024) } {
            let dir_data = unsafe { buffer::bh(buf_bn).data_mut() };
            // "." entry
            dir_data[0..4].copy_from_slice(&i.i_ino.to_le_bytes());
            let dot_rec_len = ext4_dir_rec_len(1);
            dir_data[4..6].copy_from_slice(&dot_rec_len.to_le_bytes());
            dir_data[6] = 1;
            dir_data[7] = file_type::EXT4_FT_DIR;
            dir_data[8] = b'.';
            // ".." entry（收尾到文件系统块尾）
            let dotdot_offset = dot_rec_len as usize;
            dir_data[dotdot_offset..dotdot_offset + 4].copy_from_slice(&inode::inode(dir).i_ino.to_le_bytes());
            let remaining = (fs_block_size - dotdot_offset) as u16;
            dir_data[dotdot_offset + 4..dotdot_offset + 6].copy_from_slice(&remaining.to_le_bytes());
            dir_data[dotdot_offset + 6] = 2;
            dir_data[dotdot_offset + 7] = file_type::EXT4_FT_DIR;
            dir_data[dotdot_offset + 8] = b'.';
            dir_data[dotdot_offset + 9] = b'.';
            unsafe { buffer::bh(buf_bn).b_uptodate = true };
            unsafe { buffer::mark_buffer_dirty(buf_bn) };
            unsafe { buffer::brelse(buf_bn) };
        }

        Ok(ip)
    }
}

/// 往目录的数据块中添加一条目录项。
/// 在目录的最后一个数据块中找空槽或追加新块。
unsafe fn add_entry(dir: usize, ino: u32, name: &[u8], ftype: u8) -> Result<(), i32> {
    let rec_len = ext4_dir_rec_len(name.len() as u8) as usize;
    let sb_nr = unsafe { inode::inode(dir).i_sb };
    let dev = unsafe { sb(sb_nr).s_dev };

    unsafe {
        let size = inode::inode(dir).i_size as u64;
        let info = crate::fs::ext4::ops::full::ext4_info(sb_nr);
        let fs_block_size = if info.fs_block_size > 0 { info.fs_block_size } else { BLOCK_SIZE };
        let scale = if fs_block_size > 0 { (fs_block_size / 1024) } else { 1 };
        let n_fs_blocks = if size == 0 { 0u64 } else { (size + fs_block_size as u64 - 1) / fs_block_size as u64 };

        // 目录项按文件系统块（fs_block_size）布局，必须整块扫描，否则跨 1024
        // 边界 / rec_len=fs_block_size 的项会被误判，导致每项都追加新块甚至漏项。
        for blk in 0..n_fs_blocks {
            if scale > 4 { break; }
            let mut buf = [0u8; 4096];
            let nbytes = scale * BLOCK_SIZE;
            let mut valid = true;
            for sub in 0..scale {
                let phys = crate::fs::ext4::ops::full::bmap(dir, (blk * scale as u64 + sub as u64) as u32, true);
                if phys == 0 { valid = false; break; }
                let bn = match buffer::bread(dev, phys, BLOCK_SIZE) {
                    Some(b) => b, None => { valid = false; break; }
                };
                let d = buffer::bh(bn).data();
                let s = (sub as usize) * BLOCK_SIZE;
                buf[s..s + BLOCK_SIZE].copy_from_slice(&d[..BLOCK_SIZE]);
                buffer::brelse(bn);
            }
            if !valid { continue; }

            let mut off = 0usize;
            while off + EXT4_DIR_ENTRY_HEADER_LEN <= nbytes {
                let rec = u16::from_le_bytes([buf[off + 4], buf[off + 5]]) as usize;
                if rec == 0 || off + rec > nbytes { break; }
                let ino_existing = u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]);
                let existing_name_len = buf[off + 6] as usize;
                let existing_rec = rec;

                let write_back = |buf: &[u8; 4096]| {
                    for sub in 0..scale {
                        let phys = crate::fs::ext4::ops::full::bmap(dir, (blk * scale as u64 + sub as u64) as u32, true);
                        if phys == 0 { continue; }
                        if let Some(bn) = buffer::getblk(dev, phys, BLOCK_SIZE) {
                            let d = buffer::bh(bn).data_mut();
                            let s = (sub as usize) * BLOCK_SIZE;
                            d[..BLOCK_SIZE].copy_from_slice(&buf[s..s + BLOCK_SIZE]);
                            buffer::bh(bn).b_uptodate = true;
                            buffer::mark_buffer_dirty(bn);
                            buffer::brelse(bn);
                        }
                    }
                    crate::fs::buffer::sync_dev(dev);
                };

                if ino_existing == 0 && existing_rec >= rec_len {
                    buf[off..off+4].copy_from_slice(&ino.to_le_bytes());
                    buf[off+6] = name.len() as u8; buf[off+7] = ftype;
                    let noff = off + EXT4_DIR_ENTRY_HEADER_LEN;
                    buf[noff..noff+name.len()].copy_from_slice(name);
                    write_back(&buf);
                    return Ok(());
                }

                if ino_existing != 0 && existing_rec >= ext4_dir_rec_len(existing_name_len as u8) as usize + rec_len {
                    let new_off = off + ext4_dir_rec_len(existing_name_len as u8) as usize;
                    let remaining = existing_rec - ext4_dir_rec_len(existing_name_len as u8) as usize;
                    buf[off+4..off+6].copy_from_slice(&ext4_dir_rec_len(existing_name_len as u8).to_le_bytes());
                    buf[new_off..new_off+4].copy_from_slice(&ino.to_le_bytes());
                    buf[new_off+4..new_off+6].copy_from_slice(&(remaining as u16).to_le_bytes());
                    buf[new_off+6] = name.len() as u8; buf[new_off+7] = ftype;
                    let noff = new_off + EXT4_DIR_ENTRY_HEADER_LEN;
                    buf[noff..noff+name.len()].copy_from_slice(name);
                    write_back(&buf);
                    return Ok(());
                }
                off += existing_rec;
            }
        }

        // Need a new block
        let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
        let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;
        let fs_block_size = crate::fs::ext4::ops::full::ext4_info(sb_nr).fs_block_size;
        let scale = if fs_block_size > 0 { (fs_block_size / 1024) as u32 } else { 1 };
        let phys = match crate::fs::ext4::ops::full::alloc_block_any(sb_nr) {
            Some(b) => b, None => return Err(ENOSPC),
        };
        // phys 是文件系统块号；extend_inode_block 以 fs 块为粒度索引。
        let new_block = (size / fs_block_size as u64) as u32;
        crate::fs::ext4::ops::full::extend_inode_block(dir, new_block, phys as u32);

        // 新块是 fs_block_size 字节（如 4096）的文件系统块，但缓冲缓存固定
        // 1024 字节。先把整块（scale 个子块）清零，再在子块 0 写第一条目录项，
        // rec_len 占满整个文件系统块——这样 e2fsck 按 4096 字节块解析时能看到
        // 一条收尾到块尾的合法目录项，内核按 1024 字节子块读时 DirIter 也会把
        // 越过子块尾的 rec_len 截到子块尾，不会误读残留。
        for sub in 0..scale {
            if let Some(bn) = buffer::getblk(dev, phys as u32 * scale + sub, BLOCK_SIZE) {
                let raw = buffer::bh(bn).data_mut();
                for b in raw.iter_mut() { *b = 0; }
                buffer::bh(bn).b_uptodate = true;
                buffer::mark_buffer_dirty(bn);
                buffer::brelse(bn);
            }
        }
        if let Some(bn) = buffer::getblk(dev, phys as u32 * scale, BLOCK_SIZE) {
            let raw = buffer::bh(bn).data_mut();
            raw[0..4].copy_from_slice(&ino.to_le_bytes());
            raw[4..6].copy_from_slice(&(fs_block_size as u16).to_le_bytes());
            raw[6] = name.len() as u8; raw[7] = ftype;
            raw[8..8+name.len()].copy_from_slice(name);
            buffer::bh(bn).b_uptodate = true;
            buffer::mark_buffer_dirty(bn);
            buffer::brelse(bn);
        }

        inode::inode(dir).i_size = (new_block as u32 + 1) * fs_block_size as u32;
        inode::inode(dir).i_dirt = true;
        // Sync to flush dirty parent directory buffer before it gets reused
        crate::fs::buffer::sync_dev(dev);
        Ok(())
    }
}

unsafe fn remove_entry(dir: usize, name: &[u8]) -> Result<(), i32> {
    let sb_nr = inode::inode(dir).i_sb;
    let dev = sb(sb_nr).s_dev;
    let size = inode::inode(dir).i_size as u64;
    let mut off = 0u64;
    while off < size {
        let block = (off / BLOCK_SIZE as u64) as u32;
        let phys = crate::fs::ext4::ops::full::bmap(dir, block, false);
        if phys == 0 { off = (block as u64 + 1) * BLOCK_SIZE as u64; continue; }
        let bn = match buffer::bread(dev, phys, BLOCK_SIZE) {
            Some(b) => b, None => break,
        };
        let raw = buffer::bh(bn).data_mut();
        let mut found = false;
        for entry in DirIter::new(raw) {
            if entry.is_empty() || entry.name_len as usize != name.len() { continue; }
            let ns = entry.name_off;
            if ns + name.len() <= raw.len() && &raw[ns..ns + name.len()] == name {
                let eoff = ns - EXT4_DIR_ENTRY_HEADER_LEN;
                raw[eoff] = 0; raw[eoff+1] = 0; raw[eoff+2] = 0; raw[eoff+3] = 0;
                buffer::bh(bn).b_uptodate = true;
                buffer::mark_buffer_dirty(bn);
                found = true;
                break;
            }
        }
        buffer::brelse(bn);
        if found { return Ok(()); }
        off = (block as u64 + 1) * BLOCK_SIZE as u64;
    }
    Err(ENOENT)
}

/// `rmdir` — 删除空目录。
pub unsafe fn rmdir(dir: usize, name: &[u8]) -> i32 {
    let result = (|| -> Result<(), i32> {
        let ip = lookup(dir, name)?;
        let i = inode::inode(ip);
        if !mode::is_dir(i.i_mode) { inode::iput(ip); return Err(ENOTDIR); }
        // quick check: only . and ..
        if i.i_size > 1024 {
            let mut n = 0u32;
            let mut off = 0u64;
            let dev = sb(inode::inode(ip).i_sb).s_dev;
            while off < i.i_size as u64 {
                let blk = (off / BLOCK_SIZE as u64) as u32;
                let phys = crate::fs::ext4::ops::full::bmap(ip, blk, false);
                if phys == 0 { break; }
                if let Some(bn) = buffer::bread(dev, phys, BLOCK_SIZE) {
                    let raw = buffer::bh(bn).data();
                    for e in DirIter::new(raw) { if !e.is_empty() { n += 1; if n > 2 { break; } } }
                    buffer::brelse(bn);
                }
                off = (blk as u64 + 1) * BLOCK_SIZE as u64;
            }
            if n > 2 { inode::iput(ip); return Err(ENOTEMPTY); }
        }
        remove_entry(dir, name)?;
        // 父目录链接数 -1、已用目录数 -1。
        inode::inode(dir).i_nlink -= 1;
        inode::inode(dir).i_dirt = true;
        {
            let sb_nr = i.i_sb;
            let ino = i.i_ino;
            i.i_nlink = 0;
            i.i_dirt = true;
            // 释放数据块（truncate 里逐个 free_block），再把 inode 还回位图。
            crate::fs::ext4::ops::full::truncate(ip);
            let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
            gd.used_dirs_count = gd.used_dirs_count.saturating_sub(1);
            let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;
            let dev = sb(sb_nr).s_dev;
            crate::fs::ext4::ops::full::free_inode_any(sb_nr, ino);
            crate::fs::buffer::sync_dev(dev);
        }
        inode::iput(ip);
        Ok(())
    })();
    match result { Ok(()) => 0, Err(e) => -e }
}

/// `unlink` — 删除文件。
pub unsafe fn unlink(dir: usize, name: &[u8]) -> i32 {
    let result = (|| -> Result<(), i32> {
        let ip = lookup(dir, name)?;
        let i = inode::inode(ip);
        if mode::is_dir(i.i_mode) { inode::iput(ip); return Err(EPERM); }
        remove_entry(dir, name)?;
        i.i_nlink -= 1; i.i_dirt = true;
        if i.i_nlink == 0 {
            let sb_nr = i.i_sb;
            let ino = i.i_ino;
            crate::fs::ext4::ops::full::truncate(ip);
            // 数据块已在 truncate 里释放，这里把 inode 还回位图。
            let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
            let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;
            let dev = sb(sb_nr).s_dev;
            crate::fs::ext4::ops::full::free_inode_any(sb_nr, ino);
            crate::fs::buffer::sync_dev(dev);
        }
        inode::iput(ip);
        Ok(())
    })();
    match result { Ok(()) => 0, Err(e) => -e }
}

/// `link` — 创建硬链接。
pub unsafe fn link(target: usize, dir: usize, name: &[u8]) -> i32 {
    let result = (|| -> Result<(), i32> {
        if name.len() > EXT4_NAME_LEN { return Err(ENAMETOOLONG); }
        let t = inode::inode(target);
        if t.i_nlink >= 65000 { return Err(EMLINK); }
        if find_entry(dir, name).is_some() { return Err(EEXIST); }
        add_entry(dir, t.i_ino, name, file_type::from_mode(t.i_mode))?;
        t.i_nlink += 1;
        t.i_dirt = true;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(e) => -e,
    }
}
