//! 打开/关闭文件与 fd 管理。对应 linux-1.0.9 的 `fs/open.c`
//! （`sys_open`/`sys_close`/`sys_creat`/`sys_chdir`/`sys_chmod`/…）
//! 与 `fs/fcntl.c`（`sys_dup`/`sys_dup2`）。
//!
//! # 与原版的结构性差异
//!
//! 原版 fd 表在 `current->filp[NR_OPEN]`（每进程 256 项）。我们的
//! `Task` 还没有 `filp` 字段（见 `sched/task.rs` 的取舍说明），
//! 所以 fd 表暂时是**全局**的一张 [`FD_TABLE`]，容量 [`NR_OPEN`]。
//! 这在只有内核态调用方的当下是正确的（所有代码共享一个"进程"），
//! 但 `sys_fork` 一到位就必须搬进 `Task` —— 否则父子进程会共享 fd 表
//! 的**表本身**而不是各自持有对同一批 `File` 的引用，`close` 会互相影响。
//! 搬迁时 `Task` 里加 `filp: [usize; NR_OPEN]`，`copy_process` 里
//! 逐项 `f_count += 1`（原版 `fork.c` 的 `copy_files` 就是这么做的）。
//!
//! 不移植：`sys_chown`/`sys_chmod` 的 uid 检查（没有 uid 体系）、
//! `sys_utime`、`sys_access`（都依赖 `permission()` 的完整版本）、
//! `sys_chroot`（依赖 `current->root`）。这几个的骨架留在 [`sys_chmod`]
//! 一类里，但 uid 相关的判断都标了注释。

use crate::fs::file_table::{self, filp};
use crate::fs::inode::{self, FsType, NIL};
use crate::fs::{NR_OPEN, mode, namei, oflags, super_block};
use crate::klib::errno::{EBADF, EINVAL, EMFILE, ENFILE, ENOTDIR, EROFS};
use crate::pr_info;

/// fd → 打开文件表下标。原版是 `current->filp[]`（见模块文档）。
static mut FD_TABLE: [usize; NR_OPEN] = [NIL; NR_OPEN];

/// 取某个 fd 对应的打开文件表下标，无效返回 [`NIL`]。
pub fn fd_to_filp(fd: usize) -> usize {
    if fd >= NR_OPEN {
        return NIL;
    }
    // SAFETY: 已查界；单核，改动都在进程上下文。
    unsafe { (*core::ptr::addr_of!(FD_TABLE))[fd] }
}

/// 找一个空闲 fd。对应原版 `sys_open` 里那个
/// `for(fd = 0 ; fd < NR_OPEN ; fd++) if (!current->filp[fd]) break;`。
fn get_unused_fd() -> usize {
    // SAFETY: 只读表；进程上下文。
    unsafe {
        for fd in 0..NR_OPEN {
            if (*core::ptr::addr_of!(FD_TABLE))[fd] == NIL {
                return fd;
            }
        }
        NIL
    }
}

/// 把 fd 绑到一个打开文件表项上。
///
/// # Safety
/// `fd < NR_OPEN`；进程上下文调用。
unsafe fn set_fd(fd: usize, f: usize) {
    // SAFETY: 契约转交。
    unsafe { (*core::ptr::addr_of_mut!(FD_TABLE))[fd] = f }
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
            FsType::Ext2 => 0, // TODO: ext2 open
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
        let f = fd_to_filp(oldfd);
        if f == NIL {
            return -(EBADF as i64);
        }
        if newfd >= NR_OPEN {
            return -(EBADF as i64);
        }
        // 见函数文档：同一个 fd 直接返回
        if oldfd == newfd {
            return newfd as i64;
        }
        if fd_to_filp(newfd) != NIL {
            sys_close(newfd);
        }
        filp(f).f_count += 1;
        set_fd(newfd, f);
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

/// 改权限位。对应原版 `sys_chmod()`。
///
/// # Safety
/// 只能在进程上下文调用。
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

/// 截断一个文件到指定长度。对应原版 `sys_truncate()`。
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
            if fd_to_filp(fd) != NIL {
                sys_close(fd);
            }
        }
    }
}

/// 初始化 fd 表。原版没有对应函数（`INIT_TASK` 里 `filp` 是全 NULL）。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn init() {
    // SAFETY: 契约保证独占。
    unsafe {
        for fd in 0..NR_OPEN {
            (*core::ptr::addr_of_mut!(FD_TABLE))[fd] = NIL;
        }
    }
    pr_info!("open: {} fds per process", NR_OPEN);
}

/// 已打开的 fd 数。自检用。
pub fn nr_open_fds() -> usize {
    (0..NR_OPEN).filter(|&fd| fd_to_filp(fd) != NIL).count()
}
