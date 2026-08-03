//! minix 普通文件读写。对应 linux-1.0.9 的 `fs/minix/file.c`
//! （`minix_file_read` / `minix_file_write`）。
//!
//! # 与原版的差异
//!
//! 原版 `minix_file_read` 会做预读：算出接下来要用的几个块，一次
//! `ll_rw_block(READ, nr, bhs)` 提交（那段 `for (i=0; i<read_ahead; i++)`
//! 的 `bhrequest` 数组），因为软驱/IDE 的一次寻道代价远大于多读几块。
//! 我们逐块读：唯一的块设备是 ramdisk（`memcpy`，零寻道），预读只会
//! 增加一条没有收益的代码路径。`ll_rw_block` 本身支持批量提交，
//! 接上真实磁盘驱动时把这里改回批量即可。
//!
//! 写路径的语义完整保留，包括那三个容易漏的点：
//! 1. `O_APPEND` 时位置强制取 `i_size`（在 [`super::super::read_write`] 里做）
//! 2. 写超过 `i_size` 要更新 `i_size` 并置 `i_dirt`
//! 3. **部分写一个块之前必须先把这块读进来**：否则块里未被覆盖的那部分
//!    会被当成新数据写回磁盘（原版靠 `if (c != BLOCK_SIZE && !bh->b_uptodate)`
//!    那个判断，见 [`write`] 里的对应注释）

use crate::fs::buffer::{self, BLOCK_SIZE, NIL, bh};
use crate::fs::inode;
use crate::fs::super_block::sb;
use crate::klib::errno::{EINVAL, ENOSPC};
use crate::sched;

/// 读一个普通文件。对应原版 `minix_file_read()`。
///
/// 返回读到的字节数或负 errno。
///
/// # Safety
/// 只能在进程上下文调用（会睡）。
pub unsafe fn read(n: usize, pos: u64, buf: &mut [u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let size = inode::inode(n).i_size as u64;
        if pos >= size {
            return 0; // EOF
        }
        let want = buf.len().min((size - pos) as usize);
        let mut done = 0usize;

        while done < want {
            let off = pos + done as u64;
            let block = (off / BLOCK_SIZE as u64) as u32;
            let in_block = (off % BLOCK_SIZE as u64) as usize;
            let chunk = (BLOCK_SIZE - in_block).min(want - done);

            let b = super::minix_bread(n, block, false);
            if b == NIL {
                // 文件空洞（稀疏文件）：原版对读到的空洞返回零字节
                buf[done..done + chunk].fill(0);
                done += chunk;
                continue;
            }
            buf[done..done + chunk].copy_from_slice(&bh(b).data()[in_block..in_block + chunk]);
            buffer::brelse(b);
            done += chunk;
        }

        // 原版：读也会更新 atime
        let i = inode::inode(n);
        i.i_atime = sched::current_time();
        i.i_dirt = true;
        done as i64
    }
}

/// 写一个普通文件。对应原版 `minix_file_write()`。
///
/// 返回写入的字节数或负 errno。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn write(n: usize, pos: u64, buf: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let sb_nr = inode::inode(n).i_sb;
        if sb_nr == NIL {
            return -(EINVAL as i64);
        }
        // 原版检查 s_max_size（minix v1 是 7+512+512*512 块 ≈ 64MB）
        let max = sb(sb_nr).s_max_size as u64;
        if pos >= max {
            return -(EINVAL as i64);
        }
        let want = buf.len().min((max - pos) as usize);
        let mut done = 0usize;

        while done < want {
            let off = pos + done as u64;
            let block = (off / BLOCK_SIZE as u64) as u32;
            let in_block = (off % BLOCK_SIZE as u64) as usize;
            let chunk = (BLOCK_SIZE - in_block).min(want - done);

            let b = super::get_block(n, block, true);
            if b == NIL {
                // 分不到块：已经写进去的照实返回，一个字节都没写才报错
                if done == 0 {
                    return -(ENOSPC as i64);
                }
                break;
            }
            // 见模块文档第 3 点：部分覆盖一个块，得先把原内容读进来，
            // 否则块内未覆盖的部分会写回垃圾。
            if chunk != BLOCK_SIZE && !bh(b).b_uptodate {
                crate::drivers::block::ll_rw_block(crate::fs::READ, &mut [b]);
                buffer::wait_on_buffer(b);
                if !bh(b).b_uptodate {
                    buffer::brelse(b);
                    if done == 0 {
                        return -(EINVAL as i64);
                    }
                    break;
                }
            }
            bh(b).data_mut()[in_block..in_block + chunk].copy_from_slice(&buf[done..done + chunk]);
            // 整块覆盖的情况下缓冲现在也是有效的了
            bh(b).b_uptodate = true;
            buffer::mark_buffer_dirty(b);
            buffer::brelse(b);
            done += chunk;
        }

        let end = pos + done as u64;
        let i = inode::inode(n);
        if end > i.i_size as u64 {
            i.i_size = end as u32;
        }
        i.i_mtime = sched::current_time();
        i.i_ctime = i.i_mtime;
        i.i_dirt = true;
        done as i64
    }
}
