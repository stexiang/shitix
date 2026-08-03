//! minix 目录项操作。对应 linux-1.0.9 的 `fs/minix/namei.c`。
//!
//! 实现的原版函数：`minix_match`、`minix_find_entry`、`minix_add_entry`、
//! `minix_lookup`、`minix_create`、`minix_mknod`、`minix_mkdir`、
//! `minix_rmdir`、`minix_unlink`、`minix_link`、`empty_dir`。
//!
//! 不实现 `minix_symlink`（见 `minix/mod.rs` 的说明）与 `minix_rename`
//! （原版 `do_minix_rename` 有一段很长的死锁避免逻辑：跨目录改名要
//! 同时改两个目录和被移动目录的 `..`，还要防止把目录移进自己的子树。
//! 没有 `rename` 的调用方，实现它就是纯增复杂度）。
//!
//! # 名字比较的一个坑
//!
//! 原版 `namecompare(len, maxlen, name, buffer)` 的语义是：
//! 「`name[0..len]` 与 `buffer[0..len]` 相同，**且** `buffer[len]` 是
//! NUL 或者 `len == maxlen`」。后半个条件不能少：否则查 "ab" 会匹配上
//! 目录项 "abc"。[`namecompare`] 照搬。

use crate::fs::buffer::{self, BLOCK_SIZE, NIL, bh};
use crate::fs::inode::{self, FsType};
use crate::fs::super_block::sb;
use crate::fs::{MAY_EXEC, MAY_WRITE, mode};
use crate::klib::errno::{
    EEXIST, EINVAL, EIO, EMLINK, ENAMETOOLONG, ENOENT, ENOSPC, ENOTDIR, ENOTEMPTY, EPERM,
};
use crate::sched;
use crate::{pr_warn};

use super::{DIRENT_INO_SIZE, MINIX_LINK_MAX};

/// 名字比较。见模块文档里那个坑。对应原版 `namecompare()`。
fn namecompare(name: &[u8], entry: &[u8], maxlen: usize) -> bool {
    let len = name.len();
    if len > maxlen {
        return false;
    }
    if entry.len() < len {
        return false;
    }
    if &entry[..len] != name {
        return false;
    }
    // 目录项里的名字必须正好在这里结束
    len == maxlen || entry.get(len).copied().unwrap_or(0) == 0
}

/// 在目录里找一项。对应原版 `minix_find_entry()`。
///
/// 返回 `(缓冲下标, 项在块内的偏移)`。调用方负责 `brelse` 那个缓冲。
/// 空名字当作 `"."`（原版那句注释：让 `/usr/lib//libc.a` 这种路径能用）。
///
/// # Safety
/// 只能在进程上下文调用。`dir` 是已 `iget` 的目录 inode。
pub unsafe fn find_entry(dir: usize, name: &[u8]) -> Option<(usize, usize)> {
    // SAFETY: 契约转交。
    unsafe {
        let sb_nr = inode::inode(dir).i_sb;
        if sb_nr == NIL {
            return None;
        }
        let dirsize = sb(sb_nr).s_dirsize;
        let namelen_max = sb(sb_nr).s_namelen;
        // 原版没定义 NO_TRUNCATE，所以超长名字被截断而不是报错
        let name = if name.len() > namelen_max { &name[..namelen_max] } else { name };
        let size = inode::inode(dir).i_size as u64;

        let mut off = 0u64;
        while off < size {
            let block = (off / BLOCK_SIZE as u64) as u32;
            let b = super::minix_bread(dir, block, false);
            if b == NIL {
                off = (block as u64 + 1) * BLOCK_SIZE as u64;
                continue;
            }
            let mut in_block = (off % BLOCK_SIZE as u64) as usize;
            while in_block + dirsize <= BLOCK_SIZE && off < size {
                let raw = &bh(b).data()[in_block..in_block + dirsize];
                let ino = u16::from_le_bytes([raw[0], raw[1]]);
                if ino != 0 {
                    let nraw = &raw[DIRENT_INO_SIZE..];
                    // 空名字 → "."（见函数文档）
                    let hit = if name.is_empty() {
                        nraw[0] == b'.' && (dirsize <= DIRENT_INO_SIZE + 1 || nraw[1] == 0)
                    } else {
                        namecompare(name, nraw, namelen_max)
                    };
                    if hit {
                        return Some((b, in_block));
                    }
                }
                in_block += dirsize;
                off += dirsize as u64;
            }
            buffer::brelse(b);
            if in_block + dirsize > BLOCK_SIZE {
                off = (block as u64 + 1) * BLOCK_SIZE as u64;
            }
        }
        None
    }
}

/// 往目录里加一项（inode 号先留 0，由调用方填）。
/// 对应原版 `minix_add_entry()`。
///
/// 返回 `(缓冲下标, 块内偏移)` 或负 errno。调用方要 `brelse`。
///
/// 原版一个关键细节：找到目录文件末尾还没有空位时，它靠
/// `minix_bread(dir, block, 1)` 分配新块，并且**在越过 `i_size` 的那一刻
/// 就把新项的 inode 号清 0 并扩大 `i_size`**。顺序反了会让别的进程
/// 看到一个 `i_size` 已扩大但内容是垃圾的目录项。照搬。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn add_entry(dir: usize, name: &[u8]) -> Result<(usize, usize), i32> {
    // SAFETY: 契约转交。
    unsafe {
        let sb_nr = inode::inode(dir).i_sb;
        if sb_nr == NIL {
            return Err(ENOENT);
        }
        let dirsize = sb(sb_nr).s_dirsize;
        let namelen_max = sb(sb_nr).s_namelen;
        if name.is_empty() {
            return Err(ENOENT);
        }
        if name.len() > namelen_max {
            return Err(ENAMETOOLONG);
        }

        let mut block = 0u32;
        loop {
            let b = super::get_block(dir, block, true);
            if b == NIL {
                return Err(ENOSPC);
            }
            // 新分配的块要保证内容有效（新块由 new_block 清过零并置
            // uptodate；已有的块可能还没读进来）
            if !bh(b).b_uptodate {
                crate::drivers::block::ll_rw_block(crate::fs::READ, &mut [b]);
                buffer::wait_on_buffer(b);
                if !bh(b).b_uptodate {
                    buffer::brelse(b);
                    return Err(EIO);
                }
            }

            let mut in_block = 0usize;
            while in_block + dirsize <= BLOCK_SIZE {
                let entry_end = block as u64 * BLOCK_SIZE as u64 + (in_block + dirsize) as u64;
                // 见函数文档：越过 i_size 时先清 inode 号再扩 i_size
                if entry_end > inode::inode(dir).i_size as u64 {
                    let d = bh(b).data_mut();
                    d[in_block..in_block + 2].copy_from_slice(&0u16.to_le_bytes());
                    inode::inode(dir).i_size = entry_end as u32;
                    inode::inode(dir).i_dirt = true;
                }
                let raw = &bh(b).data()[in_block..in_block + dirsize];
                let ino = u16::from_le_bytes([raw[0], raw[1]]);
                if ino != 0 {
                    // 已存在同名项
                    if namecompare(name, &raw[DIRENT_INO_SIZE..], namelen_max) {
                        buffer::brelse(b);
                        return Err(EEXIST);
                    }
                } else {
                    // 空位：写名字（不足补 0，同原版那个 for 循环）
                    let d = bh(b).data_mut();
                    for k in 0..namelen_max {
                        let off = in_block + DIRENT_INO_SIZE + k;
                        if off >= in_block + dirsize {
                            break;
                        }
                        d[off] = if k < name.len() { name[k] } else { 0 };
                    }
                    buffer::mark_buffer_dirty(b);
                    let i = inode::inode(dir);
                    i.i_mtime = sched::current_time();
                    i.i_ctime = i.i_mtime;
                    i.i_dirt = true;
                    return Ok((b, in_block));
                }
                in_block += dirsize;
            }
            buffer::brelse(b);
            block += 1;
        }
    }
}

/// 写一个目录项的 inode 号。
///
/// # Safety
/// `b` 是持有引用的缓冲，`off + 2 <= BLOCK_SIZE`。
unsafe fn set_entry_ino(b: usize, off: usize, ino: u16) {
    // SAFETY: 契约转交。
    unsafe {
        bh(b).data_mut()[off..off + 2].copy_from_slice(&ino.to_le_bytes());
        buffer::mark_buffer_dirty(b);
    }
}

/// 读一个目录项的 inode 号。
///
/// # Safety
/// 同 [`set_entry_ino`]。
unsafe fn get_entry_ino(b: usize, off: usize) -> u16 {
    // SAFETY: 契约转交。
    unsafe {
        let d = bh(b).data();
        u16::from_le_bytes([d[off], d[off + 1]])
    }
}

/// 在目录里查一个名字。对应原版 `minix_lookup()`。
///
/// 返回找到的 inode 下标（已 `iget`，调用方负责 `iput`）或负 errno。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn lookup(dir: usize, name: &[u8]) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        if !mode::is_dir(inode::inode(dir).i_mode) {
            return Err(ENOTDIR);
        }
        let sb_nr = inode::inode(dir).i_sb;
        if sb_nr == NIL {
            return Err(ENOENT);
        }
        let (b, off) = match find_entry(dir, name) {
            Some(x) => x,
            None => return Err(ENOENT),
        };
        let ino = get_entry_ino(b, off) as u32;
        buffer::brelse(b);
        if ino == 0 {
            return Err(ENOENT);
        }
        // iget 带 cross_mnt：如果这个 inode 是挂载点，返回挂上来的根
        let n = inode::iget(sb_nr, ino);
        if n == NIL { Err(ENOENT) } else { Ok(n) }
    }
}

/// 建一个普通文件。对应原版 `minix_create()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn create(dir: usize, name: &[u8], m: u16) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe { mknod(dir, name, (m & !mode::S_IFMT) | mode::S_IFREG, 0) }
}

/// 建一个任意类型的节点（普通文件、设备、FIFO）。
/// 对应原版 `minix_mknod()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn mknod(dir: usize, name: &[u8], m: u16, rdev: u16) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        if !mode::is_dir(inode::inode(dir).i_mode) {
            return Err(ENOTDIR);
        }
        if inode::inode(dir).is_rdonly() {
            return Err(EPERM);
        }
        // 原版：先查重（add_entry 也会查，但那要等分配完 inode 才发现）
        if let Some((b, _)) = find_entry(dir, name) {
            buffer::brelse(b);
            return Err(EEXIST);
        }

        let n = super::new_inode(dir);
        if n == NIL {
            return Err(ENOSPC);
        }
        {
            let i = inode::inode(n);
            i.i_mode = m;
            i.i_op = if mode::is_reg(m) || mode::is_dir(m) {
                FsType::Minix
            } else if mode::is_chr(m) {
                FsType::Chr
            } else if mode::is_blk(m) {
                FsType::Blk
            } else {
                FsType::None
            };
            if mode::is_chr(m) || mode::is_blk(m) {
                i.i_rdev = rdev;
            }
            i.i_dirt = true;
        }

        let (b, off) = match add_entry(dir, name) {
            Ok(x) => x,
            Err(e) => {
                // 原版：加不进目录就把刚分的 inode 还掉
                inode::inode(n).i_nlink = 0;
                inode::iput(n);
                return Err(e);
            }
        };
        set_entry_ino(b, off, inode::inode(n).i_ino as u16);
        buffer::brelse(b);
        Ok(n)
    }
}

/// 建目录。对应原版 `minix_mkdir()`。
///
/// 新目录里要先放好 `.` 和 `..` 两项，并把父目录的 `i_nlink` 加一
/// （因为 `..` 是一个指向父目录的链接）。这两步都不能漏：
/// 少了 `..` 的目录无法 `cd ..`；少加父目录的 nlink 会让 `rmdir`
/// 在删掉子目录后把父目录的计数减成负数。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn mkdir(dir: usize, name: &[u8], m: u16) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        if !mode::is_dir(inode::inode(dir).i_mode) {
            return Err(ENOTDIR);
        }
        if inode::inode(dir).is_rdonly() {
            return Err(EPERM);
        }
        // 原版：父目录的链接数已经到顶就不能再加子目录
        if (*inode::inode_ptr(dir)).i_nlink >= MINIX_LINK_MAX {
            return Err(EMLINK);
        }
        if let Some((b, _)) = find_entry(dir, name) {
            buffer::brelse(b);
            return Err(EEXIST);
        }

        let n = super::new_inode(dir);
        if n == NIL {
            return Err(ENOSPC);
        }
        let sb_nr = inode::inode(dir).i_sb;
        let dirsize = sb(sb_nr).s_dirsize;
        let namelen_max = sb(sb_nr).s_namelen;

        {
            let i = inode::inode(n);
            i.i_mode = (m & !mode::S_IFMT) | mode::S_IFDIR;
            i.i_op = FsType::Minix;
            // 目录一开始就有两项：. 和 ..
            i.i_size = (2 * dirsize) as u32;
            i.i_dirt = true;
        }

        // 铺第一个数据块，写 "." 与 ".."
        let db = super::get_block(n, 0, true);
        if db == NIL {
            inode::inode(n).i_nlink = 0;
            inode::iput(n);
            return Err(ENOSPC);
        }
        let (self_ino, parent_ino) =
            ((*inode::inode_ptr(n)).i_ino as u16, (*inode::inode_ptr(dir)).i_ino as u16);
        {
            let d = bh(db).data_mut();
            d[..2 * dirsize].fill(0);
            // "."
            d[0..2].copy_from_slice(&self_ino.to_le_bytes());
            d[DIRENT_INO_SIZE] = b'.';
            // ".."
            d[dirsize..dirsize + 2].copy_from_slice(&parent_ino.to_le_bytes());
            d[dirsize + DIRENT_INO_SIZE] = b'.';
            d[dirsize + DIRENT_INO_SIZE + 1] = b'.';
            let _ = namelen_max;
        }
        bh(db).b_uptodate = true;
        buffer::mark_buffer_dirty(db);
        buffer::brelse(db);

        // "." 指向自己，所以新目录的 nlink 是 2
        inode::inode(n).i_nlink = 2;
        inode::inode(n).i_dirt = true;

        let (b, off) = match add_entry(dir, name) {
            Ok(x) => x,
            Err(e) => {
                inode::inode(n).i_nlink = 0;
                inode::iput(n);
                return Err(e);
            }
        };
        set_entry_ino(b, off, self_ino);
        buffer::brelse(b);

        // ".." 是指向父目录的链接（见函数文档）
        let d = inode::inode(dir);
        d.i_nlink += 1;
        d.i_dirt = true;
        Ok(n)
    }
}

/// 目录里除了 `.` 和 `..` 还有别的项吗。对应原版 `empty_dir()`。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn empty_dir(n: usize) -> bool {
    // SAFETY: 契约转交。
    unsafe {
        let sb_nr = inode::inode(n).i_sb;
        if sb_nr == NIL {
            return false;
        }
        let dirsize = sb(sb_nr).s_dirsize as u64;
        // 原版先检查 i_size 至少能装下 . 和 ..
        if (inode::inode(n).i_size as u64) < 2 * dirsize {
            pr_warn!("minix: empty_dir: bad directory size");
            return false;
        }
        // 从第三项开始看有没有非空洞
        let mut pos = 2 * dirsize;
        while let Some(e) = super::dir::readdir(n, pos) {
            if e.ino != 0 {
                return false;
            }
            pos = e.offset + dirsize;
        }
        true
    }
}

/// 删一个目录。对应原版 `minix_rmdir()`。
///
/// 检查顺序照搬原版：目录项存在 → inode 拿到 → 是目录 → 不是挂载点 →
/// 不是当前进程的根 → 空的 → `i_count == 1`（没人在用）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn rmdir(dir: usize, name: &[u8]) -> i32 {
    // SAFETY: 契约转交。
    unsafe {
        if inode::inode(dir).is_rdonly() {
            return EPERM;
        }
        let sb_nr = inode::inode(dir).i_sb;
        let (b, off) = match find_entry(dir, name) {
            Some(x) => x,
            None => return ENOENT,
        };
        let ino = get_entry_ino(b, off) as u32;
        if ino == 0 {
            buffer::brelse(b);
            return ENOENT;
        }
        // 注意：这里要的是**不穿越挂载点**的 inode。用 iget（穿越）
        // 会拿到被挂上来的那个文件系统的根，然后把它当成本目录的
        // 子目录去删。原版这里用的是 `iget(dir->i_sb, de->inode)`
        // 而后面靠 `inode->i_mount` 检查挡住挂载点，我们直接不穿越，
        // 再显式检查 i_mount。
        let n = inode::iget_cross(sb_nr, ino, false);
        if n == NIL {
            buffer::brelse(b);
            return ENOENT;
        }

        let err = loop {
            if !mode::is_dir(inode::inode(n).i_mode) {
                break ENOTDIR;
            }
            if (*inode::inode_ptr(n)).i_mount != NIL {
                break crate::klib::errno::EBUSY;
            }
            if n == crate::fs::super_block::root_inode() {
                break crate::klib::errno::EBUSY;
            }
            if !empty_dir(n) {
                break ENOTEMPTY;
            }
            // 原版：还有别人引用着就不能删
            if (*inode::inode_ptr(n)).i_count > 1 {
                break crate::klib::errno::EBUSY;
            }
            if (*inode::inode_ptr(n)).i_nlink != 2 {
                pr_warn!("minix: empty directory has nlink != 2 ({})", inode::inode(n).i_nlink);
            }
            // 清目录项、把两个 nlink 都归零
            set_entry_ino(b, off, 0);
            {
                let i = inode::inode(n);
                i.i_nlink = 0;
                i.i_dirt = true;
            }
            {
                let d = inode::inode(dir);
                // 子目录的 ".." 消失了，父目录的 nlink 减一
                d.i_nlink -= 1;
                d.i_ctime = sched::current_time();
                d.i_mtime = d.i_ctime;
                d.i_dirt = true;
            }
            break 0;
        };
        buffer::brelse(b);
        inode::iput(n);
        err
    }
}

/// 删一个文件。对应原版 `minix_unlink()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn unlink(dir: usize, name: &[u8]) -> i32 {
    // SAFETY: 契约转交。
    unsafe {
        if inode::inode(dir).is_rdonly() {
            return EPERM;
        }
        let sb_nr = inode::inode(dir).i_sb;
        let (b, off) = match find_entry(dir, name) {
            Some(x) => x,
            None => return ENOENT,
        };
        let ino = get_entry_ino(b, off) as u32;
        if ino == 0 {
            buffer::brelse(b);
            return ENOENT;
        }
        let n = inode::iget_cross(sb_nr, ino, false);
        if n == NIL {
            buffer::brelse(b);
            return ENOENT;
        }

        let err = loop {
            // 原版：unlink 不能用在目录上（那是 rmdir 的活）
            if mode::is_dir(inode::inode(n).i_mode) {
                break EPERM;
            }
            if (*inode::inode_ptr(n)).i_nlink == 0 {
                pr_warn!(
                    "Deleting nonexistent file ({:04x}:{}), {}",
                    inode::inode(n).i_dev,
                    inode::inode(n).i_ino,
                    inode::inode(n).i_nlink
                );
                inode::inode(n).i_nlink = 1;
            }
            set_entry_ino(b, off, 0);
            {
                let i = inode::inode(n);
                i.i_nlink -= 1;
                i.i_ctime = sched::current_time();
                i.i_dirt = true;
            }
            {
                let d = inode::inode(dir);
                d.i_ctime = sched::current_time();
                d.i_mtime = d.i_ctime;
                d.i_dirt = true;
            }
            break 0;
        };
        buffer::brelse(b);
        // 这个 iput 是关键：nlink 归零时它会触发 put_inode，
        // 真正释放数据块与 inode 位图位。
        inode::iput(n);
        err
    }
}

/// 建一个硬链接。对应原版 `minix_link()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn link(target: usize, dir: usize, name: &[u8]) -> i32 {
    // SAFETY: 契约转交。
    unsafe {
        if inode::inode(dir).is_rdonly() {
            return EPERM;
        }
        // 原版：不能给目录建硬链接（会造出环）
        if mode::is_dir(inode::inode(target).i_mode) {
            return EPERM;
        }
        if (*inode::inode_ptr(target)).i_nlink >= MINIX_LINK_MAX {
            return EMLINK;
        }
        if let Some((b, _)) = find_entry(dir, name) {
            buffer::brelse(b);
            return EEXIST;
        }
        let (b, off) = match add_entry(dir, name) {
            Ok(x) => x,
            Err(e) => return e,
        };
        set_entry_ino(b, off, inode::inode(target).i_ino as u16);
        buffer::brelse(b);
        let i = inode::inode(target);
        i.i_nlink += 1;
        i.i_ctime = sched::current_time();
        i.i_dirt = true;
        0
    }
}

/// 消掉未使用告警：这几个在 `fs::namei` 的权限检查里用。
#[allow(dead_code)]
const _E: (u16, u16, i32) = (MAY_EXEC, MAY_WRITE, EINVAL);
