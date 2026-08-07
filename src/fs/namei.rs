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
//! 2. **不做符号链接展开**。原版 `_namei` 的 `follow_link` 循环
//!    （带 `current->link_count` 防环）依赖 `minix_follow_link`，
//!    而 `minix/symlink.c` 没移植（见 `minix/mod.rs` 的说明）。
//!    遇到符号链接会因为 `i_op == FsType::None` 而返回 `-EINVAL`。
//! 3. **权限检查简化**。原版 `permission()` 比对 `current->euid`/`egid`
//!    与 inode 的 uid/gid 选 owner/group/other 三组权限位，root
//!    （`suser()`）全通过。我们的 `Task` 还没有 uid 字段
//!    （见 `sched/task.rs` 的取舍说明），所以 [`permission`] 目前
//!    等价于「以 root 身份检查」：只挡 `MS_RDONLY` 挂载上的写，
//!    以及「对非目录做目录操作」这类结构性错误。uid 到位后
//!    把注释里那段补上即可。

use crate::fs::inode::{self, FsType, NIL};
use crate::fs::super_block;
use crate::fs::{MAY_EXEC, MAY_READ, MAY_WRITE, MS_RDONLY, mode, oflags};
use crate::klib::errno::{
    EACCES, EEXIST, EINVAL, EISDIR, ENAMETOOLONG, ENOENT, ENOSYS, ENOTDIR, EPERM, EROFS,
};

/// 路径分量的最大长度。对应原版 `include/linux/limits.h` 的 `NAME_MAX 255`，
/// 但 minix 最长 30，所以取 32 够用且省栈。
pub const NAME_MAX: usize = 32;

/// 权限检查。对应原版 `permission()`。见模块文档第 3 点。
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
        // 原版这里是：
        //   mode = inode->i_mode;
        //   if (current->euid == inode->i_uid) mode >>= 6;
        //   else if (in_group_p(inode->i_gid)) mode >>= 3;
        //   if (((mode & mask & 0007) == mask) || suser()) return 1;
        // 我们没有 euid（见模块文档第 3 点），等价于 suser() 恒真。
        true
    }
}

/// 把路径切成分量。返回一个迭代器，跳过连续的 `/`
/// （原版靠 `for(;;) { c = *name; if (!c) break; ... }` 里那个
/// `while (c == '/')` 达到同样效果，这让 `/usr//lib` 与 `/usr/lib` 等价）。
fn components(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.split(|&c| c == b'/').filter(|s| !s.is_empty())
}

/// 解析路径，返回**最后一个分量的父目录** inode 与那个分量的名字。
/// 对应原版 `dir_namei()`。
///
/// 返回的 inode 已 `iget`，调用方负责 `iput`。
///
/// 路径以 `/` 开头则从根开始，否则从 `pwd` 开始（同原版）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn dir_namei(path: &[u8]) -> Result<(usize, &[u8]), i32> {
    // SAFETY: 契约转交。
    unsafe {
        let start = if path.first() == Some(&b'/') {
            super_block::root_inode()
        } else {
            super_block::pwd_inode()
        };
        if start == NIL {
            return Err(ENOENT);
        }

        // 最后一个分量单独拿出来
        let mut parts: [&[u8]; 0] = [];
        let _ = &mut parts;
        let all: &[u8] = path;
        // 找最后一个 '/' 之后的部分
        let (dir_part, last) = match all.iter().rposition(|&c| c == b'/') {
            Some(p) => (&all[..p], &all[p + 1..]),
            None => (&all[..0], all),
        };

        // 从起点开始逐级 lookup
        (*inode::inode_ptr(start)).i_count += 1;
        let mut cur = start;
        for comp in components(dir_part) {
            if comp.len() > NAME_MAX {
                inode::iput(cur);
                return Err(ENAMETOOLONG);
            }
            // 用裸指针避免多个 &mut 别名
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
            inode::iput(cur);
            cur = next;
        }
        let final_mode = core::ptr::addr_of!((*inode::inode_ptr(cur)).i_mode).read_volatile();
        if !mode::is_dir(final_mode) {
            inode::iput(cur);
            return Err(ENOTDIR);
        }
        Ok((cur, last))
    }
}

/// 在一个目录里查一个分量。对应原版 `lookup()`。
///
/// 处理 `..` 的两个特例（原版 `lookup` 开头那段）：
/// 1. 在**根目录**里 `..` 就是根目录自己
/// 2. 在一个**挂载点的根**里 `..` 要跳回被盖住的那个目录所在的文件系统
///
/// 少了第 2 条，`cd /mnt/..` 会停在挂载的文件系统里出不来。
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
            if dir == super_block::root_inode() {
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
            FsType::Ext2 => super::minix::namei::lookup(dir, name), // ext4 delegates to minix ops
            _ => Err(ENOTDIR),
        }
    }
}

/// 解析一个完整路径。对应原版 `namei()`。
///
/// 返回的 inode 已 `iget`，调用方负责 `iput`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn namei(path: &[u8]) -> Result<usize, i32> {
    // SAFETY: 契约转交。
    unsafe {
        let (dir, last) = dir_namei(path)?;
        if last.is_empty() {
            // 路径以 / 结尾（或就是 "/"）：目标就是那个目录
            return Ok(dir);
        }
        let r = lookup_one(dir, last);
        inode::iput(dir);
        r
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
                inode::iput(dir);
                n
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
                    FsType::Ext2 => super::minix::namei::create(dir, last, m), // ext4 -> minix
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
            FsType::Ext2 => super::minix::namei::mknod(dir, last, m, rdev), // ext4 -> minix
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
            FsType::Ext2 => super::minix::namei::mkdir(dir, last, m), // ext4 -> minix
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
            FsType::Ext2 => super::minix::namei::rmdir(dir, last), // ext4 -> minix
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
            FsType::Ext2 => super::minix::namei::unlink(dir, last), // ext4 -> minix
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
            FsType::Ext2 => super::minix::namei::link(target, dir, last), // ext4 -> minix
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
