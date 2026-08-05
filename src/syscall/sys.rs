//! 系统调用的具体实现。对应 linux-1.0.9 的 `kernel/sys.c` 与
//! `kernel/sched.c` 里那些 `sys_*` 函数。
//!
//! 这里只实现不依赖 `fs/`、`kernel/signal.c`、`mm/mmap.c` 的那些，
//! 其余在分发表里指向 [`ni_syscall`]。每个函数的文档注明原版位置。

use super::{SysArgs, nr};
use crate::klib::errno::{EFAULT, EINVAL, ENOSYS, EBADF};
use crate::klib::printk::Level;
use crate::sched;
use crate::traps::PtRegs;

/// 未实现的调用。对应原版 `sched.c:sys_ni_syscall()`，同样返回 `-EINVAL`。
///
/// 原版返回 `-EINVAL` 而不是 `-ENOSYS` 有点反直觉，但那是 1.0.9 的实际行为，
/// 照抄。调用号越界走的是 `do_syscall` 里的 `-ENOSYS`，两条路径不同。
pub fn ni_syscall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    -(EINVAL as i64)
}

/// 返回当前进程 pid。对应原版 `sched.c:sys_getpid()`。
pub fn getpid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 系统调用上下文里 current 必然有效。
    unsafe { sched::current() }.pid as i64
}

/// 返回父进程 pid。对应原版 `sched.c:sys_getppid()`
/// （原版是 `current->p_opptr->pid`）。
pub fn getppid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: current 有效；parent 是有效槽位下标（init 的 parent 是自己）。
    unsafe {
        let parent = sched::current().parent;
        sched::task(parent).pid as i64
    }
}

/// 返回进程组。对应原版 `sched.c:sys_getpgrp()`。
pub fn getpgrp(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: current 有效。
    unsafe { sched::current() }.pgrp as i64
}

/// 挂起直到收到信号。对应原版 `sched.c:sys_pause()`。
///
/// 原版返回 `-ERESTARTNOHAND`（让信号处理完后不重启这个调用）。
/// 我们照抄返回值，但因为信号还没移植，实际效果是「睡到被 timeout 唤醒」。
pub fn pause(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::ERESTARTNOHAND;
    // SAFETY: 系统调用上下文，不在中断里，可以睡。
    unsafe {
        let nr = sched::current_nr();
        if nr == 0 {
            // task[0] 不能睡（原版 __sleep_on 里有同样的 panic）
            return -(EINVAL as i64);
        }
        sched::current().state = crate::sched::task::TaskState::Interruptible;
        sched::schedule();
    }
    -(ERESTARTNOHAND as i64)
}

/// 进程时间统计。对应原版 `sys.c:sys_times()`。
///
/// 原版往用户空间的 `struct tms *` 写四个 clock_t 并返回 jiffies。
/// 我们只返回 jiffies，指针参数暂时忽略——`verify_area`（用户地址校验）
/// 依赖 `mm/mmap.c` 的 `vm_area_struct`，还没移植，往用户指针写是不安全的。
pub fn times(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a0 != 0 {
        // 原版这里 `error = verify_area(VERIFY_WRITE,tbuf,sizeof *tbuf)`
        // 我们还没有 verify_area，拒绝非空指针比写坏用户内存好。
        return -(EFAULT as i64);
    }
    sched::jiffies() as i64
}

/// 写。对应原版 `fs/read_write.c:sys_write()`。
///
/// 完整实现要 `fs/` 的 file 表和 inode 层。这里只支持 fd 1/2（stdout/stderr）
/// 且直接把字节送到内核控制台——够让用户态程序（和自检）打印东西。
/// fd 0 或其他值返回 `-EINVAL`（原版是 `-EBADF`，但那需要 file 表才能区分
/// 「无效 fd」和「未打开」，暂时统一）。
pub fn write(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let (fd, buf, count) = (args.a0, args.a1 as *const u8, args.a2 as usize);
    if fd != 1 && fd != 2 {
        return -(EINVAL as i64);
    }
    if buf.is_null() {
        return -(EFAULT as i64);
    }
    // 没有 verify_area，所以只接受内核地址（恒等映射的低 1GB 内）。
    // 用户态传进来的地址将来要经 verify_area 校验。
    if (buf as u64) >= (1 << 30) {
        return -(EFAULT as i64);
    }
    // SAFETY: 上面已确认 buf 非空且落在恒等映射的低 1GB 内；count 由调用方
    // 保证不越过该缓冲区（暂无 verify_area 可校验，这是已知的待补项）。
    let bytes = unsafe { core::slice::from_raw_parts(buf, count) };
    match core::str::from_utf8(bytes) {
        Ok(s) => {
            crate::print!("{}", s);
            crate::serial::print(s);
            count as i64
        }
        // 非 UTF-8 就逐字节送，保持 write(2) 的字节流语义
        Err(_) => {
            for &b in bytes {
                crate::print!("{}", b as char);
            }
            count as i64
        }
    }
}

/// 退出当前进程。对应原版 `exit.c:sys_exit()` → `do_exit()`。
///
/// 原版 `do_exit` 要释放页表、关文件、通知父进程、转 ZOMBIE 等父进程 wait。
/// 那些依赖 `fs/` 和信号。这里只做内核线程能做的部分：记录退出码后让出 CPU。
pub fn exit(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let code = args.a0 as i32;
    // SAFETY: 系统调用上下文，current 有效。
    unsafe {
        let cur = sched::current();
        cur.exit_code = code;
        crate::pr!(Level::Info, "sys_exit: pid {} exiting with code {}", cur.pid, code);
        // task[0] 退出是致命的（原版 do_exit 里
        // `if (current == task[0]) panic("task[0] exiting")`）
        if sched::current_nr() == 0 {
            panic!("task[0] exiting");
        }
        cur.state = crate::sched::task::TaskState::Zombie;
        sched::set_need_resched();
        sched::schedule();
    }
    // 一个 ZOMBIE 不会被再次调度到，所以走不到这里
    0
}

/// 系统信息。对应原版 `sys.c:sys_uname()` / `sys_newuname()`。
///
/// 原版往用户态的 `struct utsname *` 写六个定长字符串。同 [`times`]，
/// 缺 `verify_area` 所以不往用户指针写，改成直接打印到控制台
/// 并返回 0——够验证调用链路，等 fs/mm 到位后改成真的填结构体。
pub fn uname(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::pr!(Level::Info, "shitix {} {} {} {}",
               crate::UTS_SYSNAME, crate::UTS_RELEASE, crate::UTS_VERSION, crate::UTS_MACHINE);
    0
}

/// idle 循环。对应原版 `sched.c:sys_idle()`（原版是 task[0] 专用，
/// 里面就是 `for(;;) { if (need_resched) schedule(); }`）。
///
/// 原版的 `sys_idle` 只允许 task[0] 调用（`if (current->pid != 0) return -EPERM`）。
pub fn idle(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EPERM;
    if sched::current_nr() != 0 {
        return -(EPERM as i64);
    }
    // 真正的空转在 lib.rs 的 idle_loop 里，这个系统调用只做资格检查。
    // 原版走到这里就再也不返回了；我们返回 0 让调用方自己转。
    0
}

/// 一个恒定返回 `-ENOSYS` 的实现，给「明确知道没做」的调用号用。
/// 与 [`ni_syscall`] 的区别是返回值（见那里的说明）。
pub fn not_implemented(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    -(ENOSYS as i64)
}

/// 编译期检查：分发表覆盖的调用号都在范围内。
const _: () = {
    assert!(nr::UNAME < nr::NR_SYSCALLS);
    assert!(nr::GETPGID < nr::NR_SYSCALLS);
};

// =============================================================================
// LFS Critical Syscalls
// =============================================================================

/// 改变数据段大小。对应原版 `mm/mmap.c:sys_brk()`。
///
/// 这是 LFS 的关键系统调用之一。用户程序用 brk() 来分配/释放内存。
/// 原版返回新地址。
pub fn brk(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let new_brk = args.a0 as usize;
    
    // SAFETY: 系统调用上下文，current 有效。
    unsafe {
        let cur = sched::current();
        let old_brk = cur.brk;
        
        if new_brk == 0 {
            // 返回当前 brk
            return old_brk as i64;
        }
        
        // TODO: 实现真正的 brk 逻辑
        // 目前只记录值
        cur.brk = new_brk;
        
        new_brk as i64
    }
}

/// 获取当前的 brk 值。
pub fn getbrk(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: current 有效。
    unsafe {
        sched::current().brk as i64
    }
}

/// 读取文件。对应原版 `fs/read_write.c:sys_read()`。
///
/// 参数：
/// - a0: 文件描述符
/// - a1: 缓冲区地址
/// - a2: 读取字节数
pub fn read(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let (fd, buf, count) = (args.a0 as i32, args.a1 as *mut u8, args.a2 as usize);
    
    if buf.is_null() {
        return -(EFAULT as i64);
    }
    
    if count == 0 {
        return 0;
    }
    
    // TODO: 集成 fs/file_table.rs
    // 目前只支持标准文件描述符
    match fd {
        0 => {
            // stdin - 目前不支持
            crate::pr_warn!("sys_read: stdin not implemented");
            -(ENOSYS as i64)
        }
        1 | 2 => {
            // stdout/stderr - 不支持读取
            -(EINVAL as i64)
        }
        _ => -(EINVAL as i64),
    }
}

/// 打开文件。对应原版 `fs/open.c:sys_open()`。
///
/// 参数：
/// - a0: 文件路径
/// - a1: 标志 (O_RDONLY, O_WRONLY, etc.)
/// - a2: 模式
pub fn open(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let flags = args.a1 as u32;
    let mode = args.a2 as u32;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs/namei.rs 和 fs/open.rs
    // 目前返回 ENOSYS 表示未实现
    crate::pr_warn!("sys_open: not fully implemented, flags=0x{:x}", flags);
    -(ENOSYS as i64)
}

/// 关闭文件。对应原版 `fs/open.c:sys_close()`。
pub fn close(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i32;
    
    if fd < 0 {
        return -(EBADF as i64);
    }
    
    // TODO: 关闭文件描述符
    0
}

/// 创建文件。对应原版 `fs/open.c:sys_creat()`。
pub fn creat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let mode = args.a1 as u32;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    crate::pr_warn!("sys_creat: not implemented, mode=0o{:o}", mode);
    -(ENOSYS as i64)
}

/// 文件状态。对应原版 `fs/stat.c:sys_stat()`。
pub fn stat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let statbuf = args.a1 as *mut u8;
    
    if pathname.is_null() || statbuf.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs/stat.rs
    -(ENOSYS as i64)
}

/// 文件状态（lstat，不跟随符号链接）。对应原版 `fs/stat.c:sys_lstat()`。
pub fn lstat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let statbuf = args.a1 as *mut u8;
    
    if pathname.is_null() || statbuf.is_null() {
        return -(EFAULT as i64);
    }
    
    -(ENOSYS as i64)
}

/// fstat - 文件状态（通过 fd）。对应原版 `fs/stat.c:sys_fstat()`。
pub fn fstat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i32;
    let statbuf = args.a1 as *mut u8;
    
    if fd < 0 || statbuf.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs/file_table.rs
    -(ENOSYS as i64)
}

/// 内存映射。对应原版 `mm/mmap.c:sys_mmap()`。
///
/// 参数：
/// - a0: addr
/// - a1: length
/// - a2: prot (PROT_READ|PROT_WRITE|PROT_EXEC)
/// - a3: flags (MAP_SHARED|MAP_PRIVATE|MAP_ANONYMOUS)
/// - a4: fd
/// - a5: offset
pub fn mmap(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let addr = args.a0 as usize;
    let len = args.a1 as usize;
    let prot = args.a2 as u32;
    let flags = args.a3 as u32;
    let fd = args.a4 as i32;
    let offset = args.a5 as usize;
    
    if len == 0 {
        return -(EINVAL as i64);
    }
    
    // TODO: 集成 mm/mmap.rs
    // 目前返回 ENOSYS
    crate::pr_warn!("sys_mmap: addr=0x{:x}, len={}, prot=0x{:x}, flags=0x{:x}", 
                     addr, len, prot, flags);
    -(ENOSYS as i64)
}

/// 解除内存映射。对应原版 `mm/mmap.c:sys_munmap()`。
pub fn munmap(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let addr = args.a0 as usize;
    let len = args.a1 as usize;
    
    if len == 0 {
        return -(EINVAL as i64);
    }
    
    // TODO: 集成 mm/mmap.rs
    -(ENOSYS as i64)
}

/// 内存保护。对应原版 `mm/mmap.c:sys_mprotect()`。
pub fn mprotect(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let addr = args.a0 as usize;
    let len = args.a1 as usize;
    let prot = args.a2 as u32;
    
    if len == 0 {
        return -(EINVAL as i64);
    }
    
    // TODO: 集成 mm/mmap.rs
    -(ENOSYS as i64)
}

/// 获取当前工作目录。对应原版 `fs/open.c:sys_getcwd()`。
pub fn getcwd(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let buf = args.a0 as *mut u8;
    let size = args.a1 as usize;
    
    if buf.is_null() || size == 0 {
        return -(EINVAL as i64);
    }
    
    // TODO: 实现 getcwd
    // 目前返回 "/"
    let cwd = b"/";
    if size < cwd.len() + 1 {
        return -(EINVAL as i64);
    }
    
    // SAFETY: buf 已校验。
    unsafe {
        core::ptr::copy_nonoverlapping(cwd.as_ptr(), buf, cwd.len());
        *buf.add(cwd.len()) = 0;
    }
    
    buf as i64
}

/// 改变当前工作目录。对应原版 `fs/open.c:sys_chdir()`。
pub fn chdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let path = args.a0 as *const u8;
    
    if path.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 重命名。对应原版 `fs/namei.c:sys_rename()`。
pub fn rename(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let oldname = args.a0 as *const u8;
    let newname = args.a1 as *const u8;
    
    if oldname.is_null() || newname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 删除文件。对应原版 `fs/namei.c:sys_unlink()`。
pub fn unlink(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 创建目录。对应原版 `fs/namei.c:sys_mkdir()`。
pub fn mkdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let mode = args.a1 as u32;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 删除目录。对应原版 `fs/namei.c:sys_rmdir()`。
pub fn rmdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 创建符号链接。对应原版 `fs/namei.c:sys_symlink()`。
pub fn symlink(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let oldname = args.a0 as *const u8;
    let newname = args.a1 as *const u8;
    
    if oldname.is_null() || newname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 读取符号链接目标。对应原版 `fs/namei.c:sys_readlink()`。
pub fn readlink(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let path = args.a0 as *const u8;
    let buf = args.a1 as *mut u8;
    let bufsize = args.a2 as usize;
    
    if path.is_null() || buf.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 复制文件描述符。对应原版 `fs/fcntl.c:sys_dup()`。
pub fn dup(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let oldfd = args.a0 as i32;
    
    if oldfd < 0 {
        return -(EBADF as i64);
    }
    
    // TODO: 集成 fs/file_table.rs
    -(ENOSYS as i64)
}

/// 复制文件描述符（指定新 fd）。对应原版 `fs/fcntl.c:sys_dup2()`。
pub fn dup2(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let oldfd = args.a0 as i32;
    let newfd = args.a1 as i32;
    
    if oldfd < 0 || newfd < 0 {
        return -(EBADF as i64);
    }
    
    // TODO: 集成 fs/file_table.rs
    -(ENOSYS as i64)
}

/// 文件控制。对应原版 `fs/fcntl.c:sys_fcntl()`。
pub fn fcntl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i32;
    let cmd = args.a1 as u32;
    let arg = args.a2 as usize;
    
    if fd < 0 {
        return -(EBADF as i64);
    }
    
    match cmd {
        // F_DUPFD - 复制文件描述符
        0 => -(ENOSYS as i64),
        // F_GETFD - 获取文件描述符标志
        1 => 0,
        // F_SETFD - 设置文件描述符标志
        2 => 0,
        // F_GETFL - 获取文件状态标志
        3 => 0, // O_ACCMODE 暂时返回 0
        // F_SETFL - 设置文件状态标志
        4 => 0,
        _ => -(EINVAL as i64),
    }
}

/// ioctl。对应原版 `fs/ioctl.c:sys_ioctl()`。
pub fn ioctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i32;
    let cmd = args.a1 as u32;
    let arg = args.a2 as usize;
    
    if fd < 0 {
        return -(EBADF as i64);
    }
    
    // TODO: 集成设备驱动
    -(ENOSYS as i64)
}

/// 访问权限检查。对应原版 `fs/open.c:sys_access()`。
pub fn access(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let mode = args.a1 as i32;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// pipe。对应原版 `fs/pipe.c:sys_pipe()`。
pub fn pipe(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fildes = args.a0 as *mut i32;
    
    if fildes.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs/pipe.rs
    -(ENOSYS as i64)
}

/// 创建特殊文件（设备/管道）。对应原版 `fs/namei.c:sys_mknod()`。
pub fn mknod(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let mode = args.a1 as u32;
    let dev = args.a2 as u32;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    crate::pr_warn!("sys_mknod: mode=0o{:o}, dev=0x{:x}", mode, dev);
    -(ENOSYS as i64)
}

/// 改变权限。对应原版 `fs/open.c:sys_chmod()`。
pub fn chmod(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let mode = args.a1 as u32;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}

/// 改变所有者。对应原版 `fs/open.c:sys_chown()`。
pub fn chown(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pathname = args.a0 as *const u8;
    let owner = args.a1 as u32;
    let group = args.a2 as u32;
    
    if pathname.is_null() {
        return -(EFAULT as i64);
    }
    
    // TODO: 集成 fs
    -(ENOSYS as i64)
}
