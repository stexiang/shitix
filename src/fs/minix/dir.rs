//! minix 目录读取。对应 linux-1.0.9 的 `fs/minix/dir.c`。
//!
//! 原版 `minix_readdir` 一次只返回一个 `struct dirent`（老式 `readdir(2)`
//! 的语义），靠 `filp->f_pos` 记住进度。照搬这个语义：`sys_getdents`
//! 那种一次返回多项的接口是 1.2 之后才有的。
//!
//! 目录本身就是一个普通文件，内容是连续的定长目录项，每项
//! `s_dirsize` 字节（16 或 32）：前 2 字节是 inode 号（小端），
//! 其余是名字（不足补 NUL，**满长度时不带 NUL 终止符**——
//! 这是最容易错的一处，所以读名字必须用 `s_namelen` 截断而不是找 NUL）。
//! inode 号为 0 表示这一项是空洞（文件被删除后留下的）。

use crate::fs::buffer::{self, BLOCK_SIZE, NIL, bh};
use crate::fs::inode;
use crate::fs::super_block::sb;
use crate::fs::{Dirent, mode};
use crate::klib::errno::{EBADF, ENOTDIR};

/// 一个目录项的解析结果。
pub struct DirEntry {
    /// inode 号，0 表示空洞
    pub ino: u16,
    /// 名字（已按 `s_namelen` 截断到第一个 NUL 之前）
    pub name: [u8; 32],
    /// 名字实际长度
    pub name_len: usize,
    /// 这一项在目录文件里的字节偏移
    pub offset: u64,
}

/// 从原始目录项字节里取出名字长度（到第一个 NUL 或 `namelen` 为止）。
/// 见模块文档：满长度时没有 NUL，所以必须先按 `namelen` 限长。
fn name_len(raw: &[u8], namelen: usize) -> usize {
    let n = namelen.min(raw.len());
    raw[..n].iter().position(|&c| c == 0).unwrap_or(n)
}

/// 读目录的下一项。对应原版 `minix_readdir()`。
///
/// `pos` 是目录内的字节偏移（原版 `filp->f_pos`），返回下一项与更新后的
/// 偏移。到末尾返回 `None`。空洞项（`ino == 0`）会被跳过，同原版。
///
/// # Safety
/// 只能在进程上下文调用。`n` 必须是已 `iget` 的目录 inode。
pub unsafe fn readdir(n: usize, pos: u64) -> Option<DirEntry> {
    // SAFETY: 契约转交。
    unsafe {
        let (sb_nr, size) = {
            let i = inode::inode(n);
            (i.i_sb, i.i_size as u64)
        };
        if sb_nr == NIL {
            return None;
        }
        let dirsize = sb(sb_nr).s_dirsize;
        let namelen = sb(sb_nr).s_namelen;
        let mut off = pos;

        while off < size {
            // 原版：目录项不跨块（dirsize 整除 BLOCK_SIZE，16|1024、32|1024）
            let block = (off / BLOCK_SIZE as u64) as u32;
            let b = super::minix_bread(n, block, false);
            if b == NIL {
                // 目录里有空洞块（稀疏目录）：跳到下一块继续
                off = (block as u64 + 1) * BLOCK_SIZE as u64;
                continue;
            }
            let data = bh(b).data();
            let mut in_block = (off % BLOCK_SIZE as u64) as usize;

            while in_block + dirsize <= BLOCK_SIZE && off < size {
                let raw = &data[in_block..in_block + dirsize];
                let ino = u16::from_le_bytes([raw[0], raw[1]]);
                if ino != 0 {
                    let nraw = &raw[super::DIRENT_INO_SIZE..];
                    let nl = name_len(nraw, namelen);
                    let mut name = [0u8; 32];
                    name[..nl].copy_from_slice(&nraw[..nl]);
                    let e = DirEntry { ino, name, name_len: nl, offset: off };
                    buffer::brelse(b);
                    return Some(e);
                }
                in_block += dirsize;
                off += dirsize as u64;
            }
            buffer::brelse(b);
            // 块尾对齐到下一块
            if in_block + dirsize > BLOCK_SIZE {
                off = (block as u64 + 1) * BLOCK_SIZE as u64;
            }
        }
        None
    }
}

/// 把一项填进用户给的 [`Dirent`]。对应原版 `minix_readdir` 末尾那段
/// `put_fs_long`/`memcpy_tofs`。
///
/// 返回下一次的 `f_pos`，或负 errno。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn fill_dirent(n: usize, pos: u64, out: &mut Dirent) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        if !mode::is_dir(inode::inode(n).i_mode) {
            return -(ENOTDIR as i64);
        }
        let sb_nr = inode::inode(n).i_sb;
        if sb_nr == NIL {
            return -(EBADF as i64);
        }
        let dirsize = sb(sb_nr).s_dirsize as u64;
        match readdir(n, pos) {
            Some(e) => {
                out.d_ino = e.ino as u64;
                out.d_off = (e.offset + dirsize) as i64;
                out.d_reclen = core::mem::size_of::<Dirent>() as u16;
                out.d_name = [0; 32];
                out.d_name[..e.name_len].copy_from_slice(&e.name[..e.name_len]);
                (e.offset + dirsize) as i64
            }
            // 到末尾：原版返回 0（读到 0 字节）
            None => 0,
        }
    }
}
