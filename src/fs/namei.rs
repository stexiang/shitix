//! 路径名解析。对应 linux-1.0.9 的 `fs/namei.c`。
//!
//! 实现的原版函数：`permission`、`lookup`、`dir_namei`、`_namei`/`namei`、
//! `lnamei`、`open_namei`、`do_mknod`、`sys_mkdir`/`sys_rmdir`/
//! `sys_unlink`/`sys_link` 的解析部分。
//!
//! # 与原版的结构性差异
//!
//! 1. **路径字符串来自内核**。原版 `namei` 的第一步是
//!    `getname(filename, &tmp)`：把用户态字符串拷进内核页
//!    （因为解析过程中会睡，用户页可能被换出）。我们目前所有调用方
//!    都在内核态（用户态要等 `execve`），所以直接收 `&[u8]`。
//!    `verify_area`/`getname` 那一层等 `mm/mmap.c` 到位再加。
//! 2. **符号链接展开**。中间分量与（默认的）末尾分量都跟随符号链接，
//!    对应原版 `_namei`/`dir_namei` 的 `follow_link` 循环（带
//!    `current->link_count` 防环，超过 5 层返回 `-ELOOP`）。ext4 的快速
//!    链接（目标内联在 `i_block`）与慢链接（目标在数据块）都支持；
//!    minix 符号链接未移植，遇到 minix 链接返回 `-EIO`。`lnamei`（`lstat`
//!    用）不跟随末尾链接。
//! 3. **权限检查**。原版 `permission()` 比对 `current->euid`/`egid`
//!    与 inode 的 uid/gid 选 owner/group/other 三组权限位，root
//!    （`suser()`）全通过。[`permission`] 先做只读文件系统写保护，
//!    再委托 [`inode::permission`] 做 uid/gid 三档位比对（读 `current()`
//!    的 euid/egid；无附加组，`in_group_p` 退化为 `egid == i_gid`）。

use crate::fs::inode::{self, FsType, NIL};
use crate::fs::super_block;
use crate::fs::{MAY_EXEC, MAY_READ, MAY_WRITE, MS_RDONLY, mode, oflags};
use crate::klib::errno::{
    EACCES, EEXIST, EINVAL, EIO, EISDIR, ELOOP, ENAMETOOLONG, ENOENT, ENOTDIR, EPERM, EROFS,
};

/// 路径分量的最大长度。对应原版 `include/linux/limits.h` 的 `NAME_MAX 255`，
/// 但 minix 最长 30，所以取 32 够用且省栈。
pub const NAME_MAX: usize = 32;

/// 权限检查。对应原版 `permission()`。见模块文档第 3 点。
///
/// 先做只读文件系统的写保护，再委托 [`inode::permission`] 做
/// owner/group/other 三档权限位比对（读 `current()` 的 euid/egid）。
///
/// # Safety
/// `n < NR_INODE` 且是有效 inode。
pub unsafe fn permission(n: usize, mask: u16) -> bool {
    // SAFETY: 契约转交。
    unsafe {
        let i = inode::inode(n);
        // 原版：只读文件系统上不许写普通文件/目录/符号链接
        // （设备文件例外——写 /dev/tty 与文件系统只读无关）
        if mask & MAY_WRITE != 0 && i.is_rdonly() {
            let m = i.i_mode;
            if mode::is_reg(m) || mode::is_dir(m) || mode::is_lnk(m) {
                return false;
            }
        }
        // 原版：
        //   mode = inode->i_mode;
        //   if (current->euid == inode->i_uid) mode >>= 6;
        //   else if (in_group_p(inode->i_gid)) mode >>= 3;
        //   if (((mode & mask & 0007) == mask) || suser()) return 1;
        inode::permission(n, mask)
    }
}

/// 把路径切成分量。返回一个迭代器，跳过连续的 `/`
/// （原版靠 `for(;;) { c = *name; if (!c) break; ... }` 里那个
/// `while (c == '/')` 达到同样效果，这让 `/usr//lib` 与 `/usr/lib` 等价）。
fn components(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.split(|&c| c == b'/').filter(|s| !s.is_empty())
}

/// 读符号链接目标。对应各 fs 的 `*_follow_link` 里取 `link` 的那一步：
/// 快速链接目标在 inode 内联区，慢链接在第一个数据块。返回目标字节切片
/// （不含 NUL）。非符号链接或读不出返回 `None`。
///
/// 目前只支持 ext4（`extra-drivers`）；minix 符号链接未移植，返回 `None`。
///
/// # Safety
/// 只能在进程上下文调用。`ip` 是已 `iget` 的 inode。
pub unsafe fn read_symlink_target(ip: usize) -> Option<([u8; 256], usize)> {
    // SAFETY: 契约转交。
    unsafe {
        let iop = core::ptr::addr_of!((*inode::inode_ptr(ip)).i_op).read_volatile();
        let im = core::ptr::addr_of!((*inode::inode_ptr(ip)).i_mode).read_volatile();
        if !mode::is_lnk(im) {
            return None;
        }
        match iop {
            FsType::Minix => super::minix::namei::read_symlink(ip),
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::ops::full::read_symlink(ip),
            _ => None,
        }
    }
}

/// 跟随一个符号链接。对应原版 `follow_link(dir, inode, flag, mode, &res)`。
///
/// `dir` 是含这个链接的目录（相对链接的解析起点），`inode` 是链接自身。
/// 若 `inode` 不是符号链接，直接返回它。否则读出目标字符串：目标以 `/`
/// 开头从根解析，否则从 `dir` 解析（同原版 `open_namei(link, ..., dir)`）。
///
/// `link_count` 防环：超过 5 层返回 `-ELOOP`（同原版）。`dir` 与 `inode`
/// 的引用计数归这个函数管：成功时返回的 inode 已 `iget`，`dir`/`inode`
/// 已 `iput`；失败时两者也已 `iput`。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn follow_link(dir: usize, inode: usize) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        if dir == NIL || inode == NIL {
            if dir != NIL { inode::iput(dir); }
            if inode != NIL { inode::iput(inode); }
            return Err(ENOENT);
        }
        let im = core::ptr::addr_of!((*inode::inode_ptr(inode)).i_mode).read_volatile();
        if !mode::is_lnk(im) {
            // 不是链接：原版返回 inode 本身，dir 释放。
            inode::iput(dir);
            return Ok(inode);
        }
        // 防环
        let lc = core::ptr::addr_of_mut!(
            (*crate::sched::task_ptr(crate::sched::current_index())).link_count
        );
        let cur_lc = lc.read_volatile();
        if cur_lc > 5 {
            inode::iput(dir);
            inode::iput(inode);
            return Err(ELOOP);
        }
        let (tgt, n) = match read_symlink_target(inode) {
            Some(v) => v,
            None => {
                inode::iput(dir);
                inode::iput(inode);
                return Err(EIO);
            }
        };
        let target = &tgt[..n];
        // 解析起点：目标以 / 开头用根，否则用 dir（含链接的目录）。
        // 原版 open_namei(link,...,dir) 里 dir 作为相对解析的 base。
        let base = if target.first() == Some(&b'/') {
            inode::iput(dir);
            super_block::task_root_inode()
        } else {
            dir
        };
        if base == NIL {
            inode::iput(inode);
            return Err(ENOENT);
        }
        lc.write_volatile(cur_lc + 1);
        let r = _namei(target, base, true);
        lc.write_volatile(cur_lc);
        // _namei 成功时返回的 inode 已 iget；它内部已 iput(base)。
        // inode（链接自身）始终释放。
        inode::iput(inode);
        r
    }
}

/// 解析路径的核心。对应原版 `_namei(path, base, follow_links, &res)`。
///
/// `base` 是相对路径的起点（已 `iget`，函数内 `iput`）。`follow_links`
/// 为真则对最终分量也跟随符号链接（`namei`/`open_namei` 行为），为假则
/// 不跟（`lnamei` 行为，用于 `lstat`/`readlink`）。
///
/// 成功返回的 inode 已 `iget`，调用方负责 `iput`。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn _namei(path: &[u8], base: usize, follow_links: bool) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        let (dir, last) = dir_namei_base(path, base)?;
        if last.is_empty() {
            // 路径以 / 结尾或就是 "/"：目标就是那个目录
            return Ok(dir);
        }
        let inode = match lookup_one(dir, last) {
            Ok(n) => n,
            Err(e) => {
                inode::iput(dir);
                return Err(e);
            }
        };
        if follow_links {
            // follow_link 会 iput(dir) 与链接 inode，返回已 iget 的目标
            follow_link(dir, inode)
        } else {
            inode::iput(dir);
            Ok(inode)
        }
    }
}

/// 解析路径，返回**最后一个分量的父目录** inode 与那个分量的名字。
/// 对应原版 `dir_namei()`。中间分量遇到符号链接会跟随（同原版）。
///
/// 返回的 inode 已 `iget`，调用方负责 `iput`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn dir_namei(path: &[u8]) -> Result<(usize, &[u8]), i32> {
    // SAFETY: 契约转交。
    unsafe {
        let start = if path.first() == Some(&b'/') {
            super_block::task_root_inode()
        } else {
            super_block::pwd_inode()
        };
        if start == NIL {
            return Err(ENOENT);
        }
        dir_namei_base(path, start)
    }
}

/// `dir_namei` 的实现，接受显式起点（已 `iget`，函数内 `iput`）。
///
/// # Safety
/// 只能在进程上下文调用。
unsafe fn dir_namei_base(path: &[u8], start: usize) -> Result<(usize, &[u8]), i32> {
    // SAFETY: 契约转交。
    unsafe {
        if start == NIL {
            return Err(ENOENT);
        }
        let all: &[u8] = path;
        // 找最后一个 '/' 之后的部分
        let (dir_part, last) = match all.iter().rposition(|&c| c == b'/') {
            Some(p) => (&all[..p], &all[p + 1..]),
            None => (&all[..0], all),
        };

        // 从起点开始逐级 lookup，中间分量跟随符号链接
        (*inode::inode_ptr(start)).i_count += 1;
        let mut cur = start;
        for comp in components(dir_part) {
            if comp.len() > NAME_MAX {
                inode::iput(cur);
                return Err(ENAMETOOLONG);
            }
            let cur_mode = core::ptr::addr_of!((*inode::inode_ptr(cur)).i_mode).read_volatile();
            if !mode::is_dir(cur_mode) {
                inode::iput(cur);
                return Err(ENOTDIR);
            }
            if !permission(cur, MAY_EXEC) {
                inode::iput(cur);
                return Err(EACCES);
            }
            let next = match lookup_one(cur, comp) {
                Ok(n) => n,
                Err(e) => {
                    inode::iput(cur);
                    return Err(e);
                }
            };
            // 中间分量若是符号链接，跟随它（原版 dir_namei 里的 follow_link）。
            // follow_link 会 iput(cur) 与链接 inode，返回已 iget 的新 cur。
            let followed = match follow_link(cur, next) {
                Ok(n) => n,
                Err(e) => return Err(e),
            };
            cur = followed;
        }
        let final_mode = core::ptr::addr_of!((*inode::inode_ptr(cur)).i_mode).read_volatile();
        if !mode::is_dir(final_mode) {
            inode::iput(cur);
            return Err(ENOTDIR);
        }
        Ok((cur, last))
    }
}

/// 在目录 `dir` 中按 inode 号反查名字（`getcwd` 的 `..` 回溯用）。
/// 对应现代内核 dcache 的 `d_parent`+`d_name` 回溯的等价物：本树没有
/// dcache，就用「扫父目录找子 inode 号」这条 1.0.9 时代的老路。
///
/// # Safety
/// 只能在进程上下文调用。`dir` 是已 `iget` 的目录 inode。
pub unsafe fn lookup_ino_name(dir: usize, ino: u32, out: &mut [u8; 255]) -> Option<usize> {
    // SAFETY: 契约转交。
    unsafe {
        let iop = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_op).read_volatile();
        match iop {
            FsType::Minix => super::minix::namei::lookup_ino(dir, ino, out),
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::namei::lookup_ino(dir, ino, out),
            _ => None,
        }
    }
}

/// 在一个目录里查一个分量。对应原版 `lookup()`。
///
/// 处理 `..` 的两个特例（原版 `lookup` 开头那段）：
/// 1. 在**根目录**里 `..` 就是根目录自己
/// 2. 在一个**挂载点的根**里 `..` 要跳回被盖住的那个目录所在的文件系统
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn lookup_one(dir: usize, name: &[u8]) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        // 用裸指针避免多个 &mut 别名
        let dir_mode = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_mode).read_volatile();
        if !mode::is_dir(dir_mode) {
            return Err(ENOTDIR);
        }
        // "." → 自己
        if name == b"." || name.is_empty() {
            (*inode::inode_ptr(dir)).i_count += 1;
            return Ok(dir);
        }
        if name == b".." {
            // 特例 1：根目录的 ..
            if dir == super_block::task_root_inode() {
                (*inode::inode_ptr(dir)).i_count += 1;
                return Ok(dir);
            }
            // 特例 2：挂载点的根 → 跳到被盖住的目录的父目录
            // 三次访问器调用要各自落地：`sb(sb_nr)` 连着取两次是两条重叠
            // 的 `&mut`（noalias UB），而 `s_covered` 读错就会把 IVT 的
            // 字节当成 inode 下标传给 iput（症状：wait_on_inode 报
            // "index 0xf000ff53f000e2c3 out of range"）。
            let sb_nr = (*inode::inode_ptr(dir)).i_sb;
            let (mounted, covered) = if sb_nr != NIL {
                let p = super_block::sb_ptr(sb_nr);
                ((*p).s_mounted, (*p).s_covered)
            } else {
                (NIL, NIL)
            };
            if sb_nr != NIL && mounted == dir {
                if covered != NIL && covered != dir {
                    (*inode::inode_ptr(covered)).i_count += 1;
                    let r = lookup_one(covered, b"..");
                    inode::iput(covered);
                    return r;
                }
            }
        }
        if !permission(dir, MAY_EXEC) {
            return Err(EACCES);
        }
        // 原版是 dir->i_op->lookup(dir, name, len, &result)
        let dir_op = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_op).read_volatile();
        match dir_op {
            FsType::Minix => super::minix::namei::lookup(dir, name),
                        #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::namei::lookup(dir, name),
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => super::minix::namei::lookup(dir, name),
            FsType::Proc => super::proc::lookup(dir, name),
            FsType::Tmpfs => super::tmpfs::lookup(dir, name),
            _ => Err(ENOTDIR),
        }
    }
}

/// 解析一个完整路径。对应原版 `namei()`，会跟随末尾的符号链接。
///
/// 返回的 inode 已 `iget`，调用方负责 `iput`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn namei(path: &[u8]) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        let start = if path.first() == Some(&b'/') {
            super_block::task_root_inode()
        } else {
            super_block::pwd_inode()
        };
        if start == NIL {
            return Err(ENOENT);
        }
        _namei(path, start, true)
    }
}

/// 解析路径但不跟随末尾符号链接。对应原版 `lnamei()`（`lstat`/`readlink` 用）。
///
/// 返回的 inode 已 `iget`，调用方负责 `iput`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn lnamei(path: &[u8]) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        let start = if path.first() == Some(&b'/') {
            super_block::task_root_inode()
        } else {
            super_block::pwd_inode()
        };
        if start == NIL {
            return Err(ENOENT);
        }
        _namei(path, start, false)
    }
}

/// 为 `open` 解析路径，按需创建。对应原版 `open_namei()`。
///
/// 返回 inode 下标（已 `iget`）或负 errno。
///
/// 原版的检查顺序照搬：
/// 1. `O_TRUNC` 隐含要写权限
/// 2. `O_CREAT|O_EXCL` 且已存在 → `-EEXIST`
/// 3. 目标是目录而想写 → `-EISDIR`
/// 4. 只读文件系统上写 → `-EROFS`
/// 5. `O_TRUNC` 真的截断
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn open_namei(path: &[u8], flags: u32, m: u16) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        let acc = flags & oflags::O_ACCMODE;
        let mut mask = match acc {
            oflags::O_RDONLY => MAY_READ,
            oflags::O_WRONLY => MAY_WRITE,
            oflags::O_RDWR => MAY_READ | MAY_WRITE,
            _ => return Err(EINVAL),
        };
        // 原版：O_TRUNC 要写权限
        if flags & oflags::O_TRUNC != 0 {
            mask |= MAY_WRITE;
        }

        let (dir, last) = dir_namei(path)?;
        if last.is_empty() {
            // 打开一个目录：只允许只读
            if mask & MAY_WRITE != 0 {
                inode::iput(dir);
                return Err(EISDIR);
            }
            return Ok(dir);
        }

        let existing = lookup_one(dir, last);
        let n = match existing {
            Ok(n) => {
                if flags & oflags::O_CREAT != 0 && flags & oflags::O_EXCL != 0 {
                    inode::iput(n);
                    inode::iput(dir);
                    return Err(EEXIST);
                }
                // O_NOFOLLOW：末尾分量是符号链接就报 ELOOP（原版
                // open_namei 的 `if (flag & O_NOFOLLOW) { iput; return -ELOOP; }`）
                if flags & oflags::O_NOFOLLOW != 0
                    && mode::is_lnk(core::ptr::addr_of!((*inode::inode_ptr(n)).i_mode).read_volatile())
                {
                    inode::iput(n);
                    inode::iput(dir);
                    return Err(ELOOP);
                }
                // 原版 open_namei 对已存在的目标 follow_link。open 拿到
                // 的应是链接指向的真实文件。
                let followed = follow_link(dir, n);
                followed?
            }
            Err(e) => {
                if e != ENOENT || flags & oflags::O_CREAT == 0 {
                    inode::iput(dir);
                    return Err(e);
                }
                // 创建。原版先查目录的写权限
                if !permission(dir, MAY_WRITE) {
                    inode::iput(dir);
                    return Err(EACCES);
                }
                let r = match inode::inode(dir).i_op {
                    FsType::Minix => super::minix::namei::create(dir, last, m),
                    #[cfg(feature = "extra-drivers")]
                    FsType::Ext2 => super::ext4::namei::create(dir, last, m),
                    #[cfg(not(feature = "extra-drivers"))]
                    FsType::Ext2 => super::minix::namei::create(dir, last, m),
                    _ => Err(ENOTDIR),
                };
                inode::iput(dir);
                let n = r?;
                // 新建的文件不需要再做下面那些检查
                return Ok(n);
            }
        };

        // 目录不能以写方式打开
        let i_ptr = inode::inode_ptr(n);
        let i_mode = core::ptr::addr_of!((*i_ptr).i_mode).read_volatile();
        if mode::is_dir(i_mode) && mask & MAY_WRITE != 0 {
            inode::iput(n);
            return Err(EISDIR);
        }
        if !permission(n, mask) {
            inode::iput(n);
            return Err(EACCES);
        }
        // 只读文件系统（设备文件例外，见 permission 的注释）
        let is_rdonly = (*i_ptr).is_rdonly();
        let is_device = (*i_ptr).is_device();
        if mask & MAY_WRITE != 0 && is_rdonly && !is_device {
            inode::iput(n);
            return Err(EROFS);
        }

        // O_TRUNC：截断到 0（全程裸指针，避免 &mut 别名 UB，bug-029）
        if flags & oflags::O_TRUNC != 0 && mode::is_reg(i_mode) {
            let i_op = core::ptr::addr_of!((*i_ptr).i_op).read_volatile();
            core::ptr::addr_of_mut!((*i_ptr).i_size).write_volatile(0);
            core::ptr::addr_of_mut!((*i_ptr).i_dirt).write_volatile(true);
            if i_op == FsType::Minix {
                super::minix::truncate::truncate(n);
            }
        }
        Ok(n)
    }
}

/// 建一个节点（设备文件等）。对应原版 `do_mknod()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn do_mknod(path: &[u8], m: u16, rdev: u16) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let (dir, last) = match dir_namei(path) {
            Ok(x) => x,
            Err(e) => return -(e as i64),
        };
        if last.is_empty() {
            inode::iput(dir);
            return -(ENOENT as i64);
        }
        if !permission(dir, MAY_WRITE) {
            inode::iput(dir);
            return -(EACCES as i64);
        }
        let dir_op = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_op).read_volatile();
        let r = match dir_op {
            FsType::Minix => super::minix::namei::mknod(dir, last, m, rdev),
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::namei::mknod(dir, last, m, rdev),
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => super::minix::namei::mknod(dir, last, m, rdev),
            _ => Err(ENOTDIR),
        };
        inode::iput(dir);
        match r {
            Ok(n) => {
                inode::iput(n);
                0
            }
            Err(e) => -(e as i64),
        }
    }
}

/// 建目录。对应原版 `sys_mkdir()` 的解析部分。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn do_mkdir(path: &[u8], m: u16) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let (dir, last) = match dir_namei(path) {
            Ok(x) => x,
            Err(e) => return -(e as i64),
        };
        if last.is_empty() {
            inode::iput(dir);
            return -(ENOENT as i64);
        }
        if !permission(dir, MAY_WRITE) {
            inode::iput(dir);
            return -(EACCES as i64);
        }
        let dir_op = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_op).read_volatile();
        let r = match dir_op {
            FsType::Minix => super::minix::namei::mkdir(dir, last, m),
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::namei::mkdir(dir, last, m),
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => super::minix::namei::mkdir(dir, last, m),
            _ => Err(ENOTDIR),
        };
        inode::iput(dir);
        match r {
            Ok(n) => {
                inode::iput(n);
                0
            }
            Err(e) => -(e as i64),
        }
    }
}

/// 建符号链接。对应原版 `sys_symlink()`：
/// dir_namei 定位父目录 → 权限检查 → 按 fs 类型分发到各 fs 的 symlink。
/// 目标不解析（允许悬空链接，原版同样不查目标存在）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn do_symlink(target: &[u8], path: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let (dir, last) = match dir_namei(path) {
            Ok(x) => x,
            Err(e) => return -(e as i64),
        };
        if last.is_empty() {
            inode::iput(dir);
            return -(ENOENT as i64);
        }
        if !permission(dir, MAY_WRITE) {
            inode::iput(dir);
            return -(EACCES as i64);
        }
        let dir_op = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_op).read_volatile();
        let r = match dir_op {
            FsType::Minix => super::minix::namei::symlink(dir, last, target),
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::namei::symlink(dir, last, target),
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => super::minix::namei::symlink(dir, last, target),
            _ => Err(ENOTDIR),
        };
        inode::iput(dir);
        match r {
            Ok(n) => {
                inode::iput(n);
                0
            }
            Err(e) => -(e as i64),
        }
    }
}

/// 删目录。对应原版 `sys_rmdir()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn do_rmdir(path: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let (dir, last) = match dir_namei(path) {
            Ok(x) => x,
            Err(e) => return -(e as i64),
        };
        if last.is_empty() {
            inode::iput(dir);
            return -(ENOENT as i64);
        }
        if !permission(dir, MAY_WRITE) {
            inode::iput(dir);
            return -(EACCES as i64);
        }
        let dir_op = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_op).read_volatile();
        let r = match dir_op {
            FsType::Minix => super::minix::namei::rmdir(dir, last),
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::namei::rmdir(dir, last),
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => super::minix::namei::rmdir(dir, last),
            _ => ENOTDIR,
        };
        inode::iput(dir);
        if r == 0 { 0 } else { -(r as i64) }
    }
}

/// 删文件。对应原版 `sys_unlink()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn do_unlink(path: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let (dir, last) = match dir_namei(path) {
            Ok(x) => x,
            Err(e) => return -(e as i64),
        };
        if last.is_empty() {
            inode::iput(dir);
            return -(ENOENT as i64);
        }
        if !permission(dir, MAY_WRITE) {
            inode::iput(dir);
            return -(EACCES as i64);
        }
        let dir_op = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_op).read_volatile();
        let r = match dir_op {
            FsType::Minix => super::minix::namei::unlink(dir, last),
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::namei::unlink(dir, last),
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => super::minix::namei::unlink(dir, last),
            _ => ENOTDIR,
        };
        inode::iput(dir);
        if r == 0 { 0 } else { -(r as i64) }
    }
}

/// 建硬链接。对应原版 `sys_link()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn do_link(oldpath: &[u8], newpath: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let target = match namei(oldpath) {
            Ok(n) => n,
            Err(e) => return -(e as i64),
        };
        // 原版：不许给目录建硬链接（会造出环，fsck 修不了）
        let target_mode = core::ptr::addr_of!((*inode::inode_ptr(target)).i_mode).read_volatile();
        if mode::is_dir(target_mode) {
            inode::iput(target);
            return -(EPERM as i64);
        }
        let (dir, last) = match dir_namei(newpath) {
            Ok(x) => x,
            Err(e) => {
                inode::iput(target);
                return -(e as i64);
            }
        };
        if last.is_empty() {
            inode::iput(target);
            inode::iput(dir);
            return -(ENOENT as i64);
        }
        // 原版：跨文件系统不能硬链接
        // target 与 dir 可能是同一个槽位；用裸指针避免两条重叠的 &mut
        if (*inode::inode_ptr(target)).i_dev != (*inode::inode_ptr(dir)).i_dev {
            inode::iput(target);
            inode::iput(dir);
            return -(EPERM as i64);
        }
        if !permission(dir, MAY_WRITE) {
            inode::iput(target);
            inode::iput(dir);
            return -(EACCES as i64);
        }
        let dir_op = core::ptr::addr_of!((*inode::inode_ptr(dir)).i_op).read_volatile();
        let r = match dir_op {
            FsType::Minix => super::minix::namei::link(target, dir, last),
            #[cfg(feature = "extra-drivers")]
            FsType::Ext2 => super::ext4::namei::link(target, dir, last),
            #[cfg(not(feature = "extra-drivers"))]
            FsType::Ext2 => super::minix::namei::link(target, dir, last),
            _ => ENOTDIR,
        };
        inode::iput(target);
        inode::iput(dir);
        if r == 0 { 0 } else { -(r as i64) }
    }
}

/// 消掉未使用告警：`MS_RDONLY` 通过 `is_rdonly` 间接用到。
#[allow(dead_code)]
const _M: u64 = MS_RDONLY;
