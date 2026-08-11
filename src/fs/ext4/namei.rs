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
/// 返回 `(缓冲下标, 项在块内的字节偏移)`。调用方负责 `brelse` 缓冲。
///
/// # Safety
/// 只能在进程上下文调用。`dir` 是已 `iget` 的目录 inode。
pub unsafe fn find_entry(dir: usize, name: &[u8]) -> Option<(usize, usize)> {
    if name.len() > EXT4_NAME_LEN {
        return None;
    }

    unsafe {
        let sb_nr = inode::inode(dir).i_sb;
        let dev = sb(sb_nr).s_dev;
        let size = inode::inode(dir).i_size as u64;
        let mut off = 0u64;
        while off < size {
            let block = (off / BLOCK_SIZE as u64) as u32;
            let phys = crate::fs::ext4::ops::full::bmap(dir, block, false);
            if phys == 0 {
                off = (block as u64 + 1) * BLOCK_SIZE as u64;
                continue;
            }

            let bn = match buffer::bread(dev, phys, BLOCK_SIZE) {
                Some(b) => b, None => break,
            };
            let data = bh(bn).data();
            let mut found = None;
            for entry in DirIter::new(data) {
                if entry.is_empty() || entry.name_len as usize != name.len() { continue; }
                let ns = entry.name_off;
                if ns + name.len() <= data.len() && &data[ns..ns + name.len()] == name {
                    found = Some((bn, entry.name_off - EXT4_DIR_ENTRY_HEADER_LEN));
                    break;
                }
            }
            if found.is_some() { return found; }
            buffer::brelse(bn);
            off = (block as u64 + 1) * BLOCK_SIZE as u64;
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
            Some((b, _off)) => {
                let raw = &bh(b).data()[_off..];
                let ino = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                buffer::brelse(b);
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
        let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
        let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;
        let ino = match bitmap::alloc_inode(sb_data, gd, 0) {
            Some(ino) => ino,
            None => return Err(ENOSPC),
        };

        // 初始化 inode
        let ip = inode::iget(sb_nr, ino);
        if ip == NIL {
            // 回退 inode 分配
            bitmap::free_inode(sb_data, gd, 0, (ino - 1) as u16);
            return Err(EIO);
        }
        let i = inode::inode(ip);
        i.i_mode = m;
        i.i_nlink = 1;
        i.i_uid = 0;  // TODO: 从当前进程获取
        i.i_gid = 0;
        i.i_size = 0;
        i.i_op = FsType::Ext2;
        i.i_sb = sb_nr;
        i.i_dirt = true;
        // data 已在初始状态全零（所有块未分配）

        // 把目录项加到目录中
        add_entry(dir, ino, name, file_type::EXT4_FT_REG_FILE)?;

        let _ = m;
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

        let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
        let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;
        let ino = match bitmap::alloc_inode(sb_data, gd, 0) {
            Some(ino) => ino,
            None => return Err(ENOSPC),
        };

        let ip = inode::iget(sb_nr, ino);
        if ip == NIL {
            bitmap::free_inode(sb_data, gd, 0, (ino - 1) as u16);
            return Err(EIO);
        }
        let i = inode::inode(ip);
        i.i_mode = m;
        i.i_nlink = 1;
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

        // 为新目录分配一个数据块并写入 "." 和 ".." 项
        let sb_nr = i.i_sb;
        let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
        let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;

        let phys = match bitmap::alloc_block(sb_data, gd, 0) {
            Some(b) => b,
            None => {
                // 回退
                i.i_nlink = 0;
                bitmap::free_inode(sb_data, gd, 0, (i.i_ino - 1) as u16);
                inode::iput(ip);
                return Err(ENOSPC);
            }
        };

        i.i_size = 1024;
        // 直接块指针：data[0] = 物理块号
        unsafe { ptr::write_volatile(&raw mut i.data[0], phys as u16); }

        // 写 ".." 和 ".." 目录项
        let dev = unsafe { sb(sb_nr).s_dev };
        let fiz = phys as u32;
        if let Some(buf_bn) = unsafe { buffer::bread(dev, fiz, 1024) } {
            let dir_data = unsafe { buffer::bh(buf_bn).data_mut() };
            // "." entry
            dir_data[0..4].copy_from_slice(&i.i_ino.to_le_bytes());
            let dot_rec_len = ext4_dir_rec_len(1);
            dir_data[4..6].copy_from_slice(&dot_rec_len.to_le_bytes());
            dir_data[6] = 1;
            dir_data[7] = file_type::EXT4_FT_DIR;
            dir_data[8] = b'.';
            // ".." entry
            let dotdot_offset = dot_rec_len as usize;
            dir_data[dotdot_offset..dotdot_offset + 4].copy_from_slice(&inode::inode(dir).i_ino.to_le_bytes());
            let remaining = (1024 - dotdot_offset) as u16;
            dir_data[dotdot_offset + 4..dotdot_offset + 6].copy_from_slice(&remaining.to_le_bytes());
            dir_data[dotdot_offset + 6] = 2;
            dir_data[dotdot_offset + 7] = file_type::EXT4_FT_DIR;
            dir_data[dotdot_offset + 8] = b'.';
            dir_data[dotdot_offset + 9] = b'.';
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
        let last_block = if size == 0 { 0u32 } else { ((size - 1) / BLOCK_SIZE as u64) as u32 };

        // Use buffer cache to read/write directory blocks
        if size > 0 {
            let phys = crate::fs::ext4::ops::full::bmap(dir, last_block, true);
            if phys != 0 {
                if let Some(bn) = buffer::bread(dev, phys, BLOCK_SIZE) {
                    let raw = buffer::bh(bn).data_mut();
                    let mut off = 0usize;
                    while off + EXT4_DIR_ENTRY_HEADER_LEN <= BLOCK_SIZE {
                        let rec = u16::from_le_bytes([raw[off + 4], raw[off + 5]]) as usize;
                        if rec == 0 || off + rec > BLOCK_SIZE { break; }
                        let ino_existing = u32::from_le_bytes([raw[off],raw[off+1],raw[off+2],raw[off+3]]);
                        let existing_name_len = raw[off + 6] as usize;
                        let existing_rec = rec;

                        if ino_existing == 0 && existing_rec >= rec_len {
                            raw[off..off+4].copy_from_slice(&ino.to_le_bytes());
                            raw[off+6] = name.len() as u8; raw[off+7] = ftype;
                            let noff = off + EXT4_DIR_ENTRY_HEADER_LEN;
                            raw[noff..noff+name.len()].copy_from_slice(name);
                            buffer::bh(bn).b_uptodate = true;
                            buffer::mark_buffer_dirty(bn);
                            buffer::brelse(bn);
                            crate::fs::buffer::sync_dev(dev);
                            return Ok(());
                        }

                        if ino_existing != 0 && existing_rec >= ext4_dir_rec_len(existing_name_len as u8) as usize + rec_len {
                            let new_off = off + ext4_dir_rec_len(existing_name_len as u8) as usize;
                            let remaining = existing_rec - ext4_dir_rec_len(existing_name_len as u8) as usize;
                            raw[off+4..off+6].copy_from_slice(&ext4_dir_rec_len(existing_name_len as u8).to_le_bytes());
                            raw[new_off..new_off+4].copy_from_slice(&ino.to_le_bytes());
                            raw[new_off+4..new_off+6].copy_from_slice(&(remaining as u16).to_le_bytes());
                            raw[new_off+6] = name.len() as u8; raw[new_off+7] = ftype;
                            let noff = new_off + EXT4_DIR_ENTRY_HEADER_LEN;
                            raw[noff..noff+name.len()].copy_from_slice(name);
                            buffer::bh(bn).b_uptodate = true;
                            buffer::mark_buffer_dirty(bn);
                            buffer::brelse(bn);
                            crate::fs::buffer::sync_dev(dev);
                            return Ok(());
                        }
                        off += existing_rec;
                    }
                    buffer::brelse(bn);
                }
            }
        }

        // Need a new block
        let gd = &mut crate::fs::ext4::ops::full::ext4_info_mut(sb_nr).ext4_gd;
        let sb_data = &crate::fs::ext4::ops::full::ext4_info(sb_nr).ext4_sb;
        let phys = match bitmap::alloc_block(sb_data, gd, 0) {
            Some(b) => b, None => return Err(ENOSPC),
        };
        let new_block = (size / BLOCK_SIZE as u64) as u32;
        crate::fs::ext4::ops::full::extend_inode_block(dir, new_block, phys as u32);

        if let Some(bn) = buffer::getblk(dev, phys as u32, BLOCK_SIZE) {
            let raw = buffer::bh(bn).data_mut();
            raw[0..4].copy_from_slice(&ino.to_le_bytes());
            raw[4..6].copy_from_slice(&((BLOCK_SIZE - rec_len) as u16).to_le_bytes());
            raw[6] = name.len() as u8; raw[7] = ftype;
            raw[8..8+name.len()].copy_from_slice(name);
            let tail = (EXT4_DIR_ENTRY_HEADER_LEN + name.len() + 3) & !3;
            if tail + EXT4_DIR_ENTRY_HEADER_LEN <= BLOCK_SIZE {
                raw[tail+4..tail+6].copy_from_slice(&((BLOCK_SIZE - tail) as u16).to_le_bytes());
            }
            buffer::bh(bn).b_uptodate = true;
            buffer::mark_buffer_dirty(bn);
            buffer::brelse(bn);
        }

        inode::inode(dir).i_size = (new_block as u32 + 1) * 1024;
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
        i.i_nlink = 0; i.i_dirt = true;
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
        if i.i_nlink == 0 { crate::fs::ext4::ops::full::truncate(ip); }
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
