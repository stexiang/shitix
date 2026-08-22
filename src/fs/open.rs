//! 打开/关闭文件与 fd 管理。对应 linux-1.0.9 的 `fs/open.c`
//! （`sys_open`/`sys_close`/`sys_creat`/`sys_chdir`/`sys_chmod`/…）
//! 与 `fs/fcntl.c`（`sys_dup`/`sys_dup2`）。
//!
//! # 与原版的结构性差异
//!
//! fd 表现在是 **per-task** 的（`Task::filp[NR_OPEN]`，原版 `current->filp`）。
//! fork 时整表复制，每项对 File 的 `f_count` +1；close 减引用计数。
//! 纯内核线程（task[0]、worker 等）不使用 fd 表。
//!
//! 不移植：`sys_chown`/`sys_chmod` 的完整 uid/gid 权限矩阵（只有
//! root 放行 + 属主本人的简化版）、`sys_utime`（无 RTC，忽略）、
//! `sys_chroot`（依赖 `current->root`）。uid/gid 体系就位后，
//! `sys_chown`/`sys_access` 已按原版实现（见 [`sys_chown`]/[`sys_access`]）。

use crate::fs::file_table::{self, filp};
use crate::fs::inode::{self, FsType, NIL};
use crate::fs::{NR_OPEN, mode, namei, oflags, super_block};
use crate::klib::errno::{EACCES, EBADF, EINVAL, EMFILE, ENFILE, ENOTDIR, EPERM, EROFS};
use crate::pr_info;

/// 每任务 FD 表（旁路数组，不在 Task 里省 BSS）。
static mut TASK_FILP: [[usize; NR_OPEN]; crate::sched::NR_TASKS] =
    [[NIL; NR_OPEN]; crate::sched::NR_TASKS];

/// 取当前任务某个 fd 对应的打开文件表下标，无效返回 [`NIL`]。
pub fn fd_to_filp(fd: usize) -> usize {
    if fd >= NR_OPEN { return NIL; }
    // SAFETY: 进程上下文，单核。
    unsafe { TASK_FILP[crate::sched::current_index()][fd] }
}

/// 设当前任务的 fd → filp 映射。
fn set_fd_to_filp(fd: usize, filp_idx: usize) {
    if fd < NR_OPEN {
        unsafe { TASK_FILP[crate::sched::current_index()][fd] = filp_idx }
    }
}

/// 设指定任务的 fd → filp 映射（供 fork 用）。
pub fn set_task_fd(task_idx: usize, fd: usize, filp_idx: usize) {
    if fd < NR_OPEN && task_idx < crate::sched::NR_TASKS {
        unsafe { TASK_FILP[task_idx][fd] = filp_idx }
    }
}

/// 取指定任务的 fd。
pub fn task_fd(task_idx: usize, fd: usize) -> usize {
    if fd >= NR_OPEN || task_idx >= crate::sched::NR_TASKS { return NIL; }
    unsafe { TASK_FILP[task_idx][fd] }
}

/// fork 时复制 fd 表。
pub fn clone_fds(from: usize, to: usize) {
    if from < crate::sched::NR_TASKS && to < crate::sched::NR_TASKS && from != to {
        unsafe {
            core::ptr::copy_nonoverlapping(
                &raw const TASK_FILP[from], &raw mut TASK_FILP[to], 1);
        }
    }
}

/// 找一个空闲 fd。对应原版 `sys_open` 里那个
/// `for(fd = 0 ; fd < NR_OPEN ; fd++) if (!current->filp[fd]) break;`。
pub fn get_unused_fd() -> usize {
    unsafe {
        let nr = crate::sched::current_index();
        for fd in 0..NR_OPEN {
            // 必须同时避开管道/socket fd：管道 fd 不在 TASK_FILP 里（只挂在
            // PIPE_FD_MAP），单看 TASK_FILP==NIL 会把已被管道占用的 fd 分出去，
            // 覆盖管道读端（GNU bash 管道：cat exec 后 ld.so open ld.so.cache
            // 拿到 fd0，把管道读端顶掉，cat 再 read(0) 读到的是 ld.so.cache 的
            // EOF，整个管道断掉）。
            if TASK_FILP[nr][fd] == NIL
                && !crate::fs::pipe::fd_is_pipe(fd)
                && !crate::net::socket::fd_is_socket(fd)
            {
                return fd;
            }
        }
        NIL
    }
}

/// 把 fd 绑到一个打开文件表项上。操作当前任务的 filp 表。
///
/// # Safety
/// `fd < NR_OPEN`；进程上下文调用。
pub unsafe fn set_fd(fd: usize, f: usize) {
    set_fd_to_filp(fd, f);
}

/// 打开一个文件。对应原版 `sys_open()`。
///
/// 返回 fd 或负 errno。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_open(path: &[u8], flags: u32, m: u16) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let fd = get_unused_fd();
        if fd == NIL {
            return -(EMFILE as i64);
        }
        let f = file_table::get_empty_filp();
        if f == NIL {
            return -(ENFILE as i64);
        }
        // 原版：先占住 fd 再 open_namei，因为后者会睡
        set_fd(fd, f);

        let n = match namei::open_namei(path, flags, m) {
            Ok(n) => n,
            Err(e) => {
                set_fd(fd, NIL);
                filp(f).f_count = 0;
                return -(e as i64);
            }
        };

        // 用裸指针避免产生多个 &mut 别名（buglog bug-029 根因）
        let i_ptr = inode::inode_ptr(n);
        let (i_op, i_rdev, i_size) = unsafe {
            (
                core::ptr::addr_of!((*i_ptr).i_op).read_volatile(),
                core::ptr::addr_of!((*i_ptr).i_rdev).read_volatile(),
                core::ptr::addr_of!((*i_ptr).i_size).read_volatile(),
            )
        };

        {
            let fp = filp(f);
            fp.f_flags = flags;
            // 原版：`f->f_mode = (flags+1) & O_ACCMODE`
            // O_RDONLY=0 → mode 1(读)，O_WRONLY=1 → 2(写)，O_RDWR=2 → 3(读写)
            fp.f_mode = ((flags + 1) & oflags::O_ACCMODE) as u16;
            fp.f_inode = n;
            fp.f_pos = 0;
            fp.f_reada = 0;
            fp.f_rdev = i_rdev;
        }

        // 设备文件要走驱动的 open（原版 chrdev_open/blkdev_open，
        // 由 def_chr_fops.open 转过来）
        let r = match i_op {
            FsType::Chr => super::devices::chrdev_open(i_rdev),
            FsType::Blk => super::devices::blkdev_open(i_rdev),
            FsType::Minix => 0,
            FsType::Ext2 => 0,
            FsType::Proc => 0,
            FsType::Tmpfs => 0,
            FsType::None => -(EINVAL as i64),
        };
        if r < 0 {
            set_fd(fd, NIL);
            filp(f).f_inode = NIL;
            filp(f).f_count = 0;
            inode::iput(n);
            return r;
        }

        // O_APPEND：位置直接到末尾（原版在 sys_write 里每次都重取，
        // 见 read_write.rs 的注释）
        if flags & oflags::O_APPEND != 0 {
            filp(f).f_pos = i_size as u64;
        }
        fd as i64
    }
}

/// 建并打开一个文件。对应原版 `sys_creat()`
/// （就是 `sys_open(path, O_CREAT|O_WRONLY|O_TRUNC, mode)`）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_creat(path: &[u8], m: u16) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        sys_open(path, oflags::O_CREAT | oflags::O_WRONLY | oflags::O_TRUNC, m)
    }
}

/// 关一个 fd。对应原版 `sys_close()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_close(fd: usize) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        set_fd(fd, NIL);
        file_table::put_filp(f);
        0
    }
}

/// 复制一个 fd。对应原版 `sys_dup()` / `fcntl.c` 的 `dupfd()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_dup(fd: usize) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        // 管道 fd：复制管道注册。目标 fd 必须同时不在管道表和普通 fd 表里。
        if crate::fs::pipe::fd_is_pipe(fd) {
            let nr = crate::sched::current_index();
            let mut new = NIL;
            for cand in 0..NR_OPEN {
                if !crate::fs::pipe::fd_is_pipe(cand)
                    && !crate::net::socket::fd_is_socket(cand)
                    && task_fd(nr, cand) == NIL
                {
                    new = cand;
                    break;
                }
            }
            if new == NIL {
                return -(EMFILE as i64);
            }
            if crate::fs::pipe::dup_fd(fd, new) {
                return new as i64;
            }
            return -(EBADF as i64);
        }
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        let new = get_unused_fd();
        if new == NIL {
            return -(EMFILE as i64);
        }
        filp(f).f_count += 1;
        set_fd(new, f);
        new as i64
    }
}

/// 复制到指定 fd。对应原版 `sys_dup2()`。
///
/// 原版语义：目标 fd 已打开就先关掉；`oldfd == newfd` 时直接返回
/// （**不**关闭）。后者容易漏，漏了会把唯一的引用关掉。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_dup2(oldfd: usize, newfd: usize) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        if newfd >= NR_OPEN {
            return -(EBADF as i64);
        }
        let old_is_pipe = crate::fs::pipe::fd_is_pipe(oldfd);
        let f = if old_is_pipe { NIL } else { fd_to_filp(oldfd) };
        if !old_is_pipe && f == NIL {
            return -(EBADF as i64);
        }
        // 见函数文档：同一个 fd 直接返回（但先确认 oldfd 有效）
        if oldfd == newfd {
            return newfd as i64;
        }
        // 关掉目标 fd（可能是管道或普通文件）
        if crate::fs::pipe::fd_is_pipe(newfd) {
            crate::fs::pipe::close_fd(newfd);
        } else if fd_to_filp(newfd) != NIL {
            sys_close(newfd);
        }
        if old_is_pipe {
            crate::fs::pipe::dup_fd(oldfd, newfd);
        } else {
            filp(f).f_count += 1;
            set_fd(newfd, f);
        }
        newfd as i64
    }
}

/// 换工作目录。对应原版 `sys_chdir()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_chdir(path: &[u8]) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let n = match namei::namei(path) {
            Ok(n) => n,
            Err(e) => return -(e as i64),
        };
        // 用裸指针避免多个 &mut 别名
        let i_mode = core::ptr::addr_of!((*inode::inode_ptr(n)).i_mode).read_volatile();
        if !mode::is_dir(i_mode) {
            inode::iput(n);
            return -(ENOTDIR as i64);
        }
        // 原版还查 permission(inode, MAY_EXEC)
        if !namei::permission(n, super::MAY_EXEC) {
            inode::iput(n);
            return -(EINVAL as i64);
        }
        // 旧的 pwd 引用要还掉（原版 `iput(current->pwd)`）
        let old = super_block::pwd_inode();
        super_block::set_pwd(n);
        if old != NIL && old != super_block::root_inode() {
            inode::iput(old);
        }
        0
    }
}

/// 按 fd 切换当前工作目录。对应原版 `fs/open.c:sys_fchdir()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_fchdir(fd: usize) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        let n = filp(f).f_inode;
        if n == NIL {
            return -(EBADF as i64);
        }
        let i_mode = core::ptr::addr_of!((*inode::inode_ptr(n)).i_mode).read_volatile();
        if !mode::is_dir(i_mode) {
            return -(ENOTDIR as i64);
        }
        if !namei::permission(n, super::MAY_EXEC) {
            return -(EINVAL as i64);
        }
        // pwd 是新增的一个引用：i_count +1（fd 的引用保持不变）。
        // SAFETY: n 被打开文件持有，有效。
        unsafe { (*inode::inode_ptr(n)).i_count += 1; }
        let old = super_block::pwd_inode();
        super_block::set_pwd(n);
        if old != NIL && old != super_block::root_inode() {
            inode::iput(old);
        }
        0
    }
}

/// 按 fd 截断文件。对应原版 `fs/open.c:sys_ftruncate()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_ftruncate(fd: usize, length: u32) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        let n = filp(f).f_inode;
        if n == NIL {
            return -(EBADF as i64);
        }
        let i_ptr = inode::inode_ptr(n);
        let i_mode = core::ptr::addr_of!((*i_ptr).i_mode).read_volatile();
        if mode::is_dir(i_mode) {
            return -(EINVAL as i64);
        }
        // 原版要查文件以写模式打开；这里简化成 permission(MAY_WRITE)。
        if !namei::permission(n, super::MAY_WRITE) {
            return -(EINVAL as i64);
        }
        let now = crate::sched::current_time();
        core::ptr::addr_of_mut!((*i_ptr).i_size).write_volatile(length);
        core::ptr::addr_of_mut!((*i_ptr).i_mtime).write_volatile(now);
        core::ptr::addr_of_mut!((*i_ptr).i_ctime).write_volatile(now);
        core::ptr::addr_of_mut!((*i_ptr).i_dirt).write_volatile(true);
        0
    }
}
pub unsafe fn sys_chmod(path: &[u8], m: u16) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let n = match namei::namei(path) {
            Ok(n) => n,
            Err(e) => return -(e as i64),
        };
        // 全程用裸指针，避免 &mut 别名 UB
        let i_ptr = inode::inode_ptr(n);
        let i_sb = core::ptr::addr_of!((*i_ptr).i_sb).read_volatile();
        if i_sb != NIL {
            let sb_flags = core::ptr::addr_of!((*super_block::sb_ptr(i_sb)).s_flags).read_volatile();
            if sb_flags & crate::fs::MS_RDONLY != 0 {
                inode::iput(n);
                return -(EROFS as i64);
            }
        }
        // 原版：`if (current->euid != inode->i_uid && !suser())
        //          { iput(inode); return -EPERM; }`
        // 没有 euid（见 namei.rs 文档第 3 点），等价于 suser() 恒真。
        let old_mode = core::ptr::addr_of!((*i_ptr).i_mode).read_volatile();
        let new_mode = (m & 0o7777) | (old_mode & mode::S_IFMT);
        let now = crate::sched::current_time();
        core::ptr::addr_of_mut!((*i_ptr).i_mode).write_volatile(new_mode);
        core::ptr::addr_of_mut!((*i_ptr).i_ctime).write_volatile(now);
        core::ptr::addr_of_mut!((*i_ptr).i_dirt).write_volatile(true);
        inode::iput(n);
        0
    }
}

/// 对已持有的 inode 执行 chown 的核心逻辑。**不** `iput`，由调用方负责。
///
/// # Safety
/// `n` 是已 `iget`（或由 fd 引用持有）的有效 inode。
unsafe fn chown_inode(n: usize, uid: u32, gid: u32) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        // 全程裸指针，避免 &mut 别名 UB
        let i_ptr = inode::inode_ptr(n);
        let i_sb = core::ptr::addr_of!((*i_ptr).i_sb).read_volatile();
        if i_sb != NIL {
            let sb_flags =
                core::ptr::addr_of!((*super_block::sb_ptr(i_sb)).s_flags).read_volatile();
            if sb_flags & crate::fs::MS_RDONLY != 0 {
                return -(EROFS as i64);
            }
        }
        let (old_uid, old_gid) = (
            core::ptr::addr_of!((*i_ptr).i_uid).read_volatile() as u32,
            core::ptr::addr_of!((*i_ptr).i_gid).read_volatile() as u32,
        );
        // 原版：`(current->euid == inode->i_uid && ...) || suser()`。
        // 简化：root 放行；属主本人只能改 gid（改属主仍需 root）。
        let euid = crate::sched::current().euid;
        if euid != 0 && euid != old_uid {
            return -(EPERM as i64);
        }
        let new_uid = if uid == u32::MAX { old_uid } else { uid & 0xFFFF };
        let new_gid = if gid == u32::MAX { old_gid } else { gid & 0xFFFF };
        let mut new_mode = core::ptr::addr_of!((*i_ptr).i_mode).read_volatile();
        if new_uid != old_uid {
            new_mode &= !mode::S_ISUID;
        }
        if new_gid != old_gid {
            new_mode &= !mode::S_ISGID;
        }
        let now = crate::sched::current_time();
        core::ptr::addr_of_mut!((*i_ptr).i_uid).write_volatile(new_uid as u16);
        core::ptr::addr_of_mut!((*i_ptr).i_gid).write_volatile(new_gid as u16);
        core::ptr::addr_of_mut!((*i_ptr).i_mode).write_volatile(new_mode);
        core::ptr::addr_of_mut!((*i_ptr).i_ctime).write_volatile(now);
        core::ptr::addr_of_mut!((*i_ptr).i_dirt).write_volatile(true);
        0
    }
}

/// 改变属主。对应原版 `sys_chown()`（原版用 `lnamei`）与 `sys_fchown()`。
///
/// `uid`/`gid` 为 `u32::MAX`（即 `-1`）表示「不变更该项」。
/// `follow` 为 false 时用 `lnamei`（不跟随末尾符号链接）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_chown(path: &[u8], uid: u32, gid: u32, follow: bool) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let n = if follow {
            match namei::namei(path) {
                Ok(n) => n,
                Err(e) => return -(e as i64),
            }
        } else {
            match namei::lnamei(path) {
                Ok(n) => n,
                Err(e) => return -(e as i64),
            }
        };
        // SAFETY: n 已 iget。
        let r = unsafe { chown_inode(n, uid, gid) };
        inode::iput(n);
        r
    }
}

/// 按 fd 改变属主。对应原版 `sys_fchown()`。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_fchown(fd: usize, uid: u32, gid: u32) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let f = fd_to_filp(fd);
        if f == NIL {
            return -(EBADF as i64);
        }
        let n = filp(f).f_inode;
        if n == NIL {
            return -(EBADF as i64);
        }
        // 该 inode 由 fd 的打开文件引用持有，直接改，不 iget/iput。
        // SAFETY: n 被打开文件持有，有效。
        unsafe { chown_inode(n, uid, gid) }
    }
}

/// 访问性检查。对应原版 `sys_access()`。
///
/// 用**真实** uid/gid（`access(2)` 语义），除非 `use_effective` 为 true
/// （`faccessat(AT_EACCESS)` 语义，用有效 uid/gid）。
/// `mode` 只允许 `R_OK|W_OK|X_OK`（`F_OK`=0 恒通过）。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_access(path: &[u8], mode: u16, use_effective: bool) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        if mode & !0o7 != 0 {
            return -(EINVAL as i64);
        }
        let n = match namei::namei(path) {
            Ok(n) => n,
            Err(e) => return -(e as i64),
        };
        let i_ptr = inode::inode_ptr(n);
        let (i_mode, i_uid, i_gid) = (
            core::ptr::addr_of!((*i_ptr).i_mode).read_volatile(),
            core::ptr::addr_of!((*i_ptr).i_uid).read_volatile() as u32,
            core::ptr::addr_of!((*i_ptr).i_gid).read_volatile() as u32,
        );
        // SAFETY: 进程上下文，单核。
        let (uid, gid) = {
            let c = crate::sched::current();
            if use_effective { (c.euid, c.egid) } else { (c.uid, c.gid) }
        };
        // 原版：`res = i_mode & S_IRWXUGO;` 然后按 owner/group/other 移位，
        // `(res & mode) == mode` 即放行；root 有单独的兜底分支。
        let mut m = i_mode;
        if uid == i_uid {
            m >>= 6;
        } else if gid == i_gid {
            m >>= 3;
        }
        inode::iput(n);
        if m & mode & 0o007 == mode {
            return 0;
        }
        // 原版 root 兜底：目录放行；非 X_OK 放行；文件有任意 x 位放行 X_OK。
        if uid == 0
            && (mode::is_dir(i_mode) || mode & 0o001 == 0 || i_mode & 0o111 != 0)
        {
            return 0;
        }
        -(EACCES as i64)
    }
}
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn sys_truncate(path: &[u8], length: u32) -> i64 {
    // SAFETY: 契约转交。
    unsafe {
        let n = match namei::namei(path) {
            Ok(n) => n,
            Err(e) => return -(e as i64),
        };
        // 全程用裸指针，避免 &mut 别名 UB（bug-029）
        let i_ptr = inode::inode_ptr(n);
        let (i_mode, i_op) = (
            core::ptr::addr_of!((*i_ptr).i_mode).read_volatile(),
            core::ptr::addr_of!((*i_ptr).i_op).read_volatile(),
        );
        if mode::is_dir(i_mode) {
            inode::iput(n);
            return -(EINVAL as i64);
        }
        if !namei::permission(n, super::MAY_WRITE) {
            inode::iput(n);
            return -(EINVAL as i64);
        }
        // 修改 inode 字段全用 write_volatile
        let now = crate::sched::current_time();
        core::ptr::addr_of_mut!((*i_ptr).i_size).write_volatile(length);
        core::ptr::addr_of_mut!((*i_ptr).i_mtime).write_volatile(now);
        core::ptr::addr_of_mut!((*i_ptr).i_ctime).write_volatile(now);
        core::ptr::addr_of_mut!((*i_ptr).i_dirt).write_volatile(true);
        if i_op == FsType::Minix {
            super::minix::truncate::truncate(n);
        }
        inode::iput(n);
        0
    }
}

/// 关掉所有 fd。对应原版 `do_exit()` 里那个
/// `for (i=0 ; i<NR_OPEN ; i++) if (current->filp[i]) sys_close(i);`。
/// `exit.rs` 移植后由那边调用。
///
/// # Safety
/// 只能在进程上下文调用。
pub unsafe fn close_all() {
    // SAFETY: 契约转交。
    unsafe {
        for fd in 0..NR_OPEN {
            // 管道 fd 也要关（否则退出后 writers/readers 永不归零，父进程
            // 的 pipe_read 读不到 EOF 会永久睡死）。
            if crate::fs::pipe::fd_is_pipe(fd) {
                crate::fs::pipe::close_fd(fd);
            }
            if crate::net::socket::fd_is_socket(fd) {
                crate::net::socket::close_socket(fd);
            }
            if fd_to_filp(fd) != NIL {
                sys_close(fd);
            }
        }
    }
}

/// 初始化 fd 表。每个 task 在创建时由 Task::empty() 初始化。
pub unsafe fn init() {
    pr_info!("open: {} fds per process", NR_OPEN);
}

/// 已打开的 fd 数（当前任务）。自检用。
pub fn nr_open_fds() -> usize {
    let nr = crate::sched::current_index();
    (0..NR_OPEN).filter(|&fd| fd_to_filp(fd) != NIL).count()
}
