//! 系统调用的具体实现。对应 linux-1.0.9 的 `kernel/sys.c` 与
//! `kernel/sched.c` 里那些 `sys_*` 函数。
//!
//! 这里只实现不依赖 `fs/`、`kernel/signal.c`、`mm/mmap.c` 的那些，
//! 其余在分发表里指向 [`ni_syscall`]。每个函数的文档注明原版位置。

use super::{SysArgs, nr};
use crate::klib::errno::{EFAULT, EINVAL, ENOSYS, EBADF, EPERM, ERANGE, EINTR};
use crate::klib::printk::Level;
use crate::sched;
use crate::traps::PtRegs;

/// 时间值结构
#[repr(C)]
pub struct TimeVal {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

/// 时区结构
#[repr(C)]
pub struct Timezone {
    pub tz_minuteswest: i32,
    pub tz_dsttime: i32,
}

/// 系统信息结构
#[repr(C)]
pub struct SysInfo {
    pub uptime: i64,
    pub loads: [u64; 3],
    pub totalram: u64,
    pub freeram: u64,
    pub sharedram: u64,
    pub bufferram: u64,
    pub totalswap: u64,
    pub freeswap: u64,
    pub procs: u64,
}

/// 资源使用情况
#[repr(C)]
pub struct RUsage {
    pub ru_utime: TimeVal,
    pub ru_stime: TimeVal,
}

/// 资源限制
#[repr(C)]
pub struct RLimit {
    pub rlim_cur: u64,
    pub rlim_max: u64,
}

/// poll 文件描述符
#[repr(C)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// 进程时间统计
#[repr(C)]
pub struct Tms {
    pub tms_utime: i64,
    pub tms_stime: i64,
    pub tms_cutime: i64,
    pub tms_cstime: i64,
}

/// 文件状态结构
#[repr(C)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub _pad0: i32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atimensec: i64,
    pub st_mtime: i64,
    pub st_mtimensec: i64,
    pub st_ctime: i64,
    pub st_ctimensec: i64,
    pub _unused: [i64; 3],
}

/// 未实现的调用。对应原版 `sched.c:sys_ni_syscall()`，同样返回 `-EINVAL`。
///
/// 原版返回 `-EINVAL` 而不是 `-ENOSYS` 有点反直觉，但那是 1.0.9 的实际行为，
/// 照抄。调用号越界走的是 `do_syscall` 里的 `-ENOSYS`，两条路径不同。
pub fn ni_syscall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    -(EINVAL as i64)
}

/// 执行程序。对应原版 `fs/exec.c:sys_execve()`。
///
/// 参数：
/// - a0: 文件名
/// - a1: 参数数组
/// - a2: 环境变量数组
pub fn execve(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // TODO: 实现真正的程序加载
    // 需要：
    // 1. 解析 ELF 文件格式
    // 2. 分配用户内存空间
    // 3. 加载代码段和数据段
    // 4. 设置栈
    // 5. 切换到用户态
    crate::pr_warn!("sys_execve: not implemented");
    -(ENOSYS as i64)
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

/// 终止进程信号。对应原版 `kernel/signal.c:sys_kill()`。
pub fn kill(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    let sig = args.a1 as i32;
    crate::pr_warn!("sys_kill: pid={}, sig={} not fully implemented", pid, sig);
    -(ENOSYS as i64)
}

/// 设置 alarm。对应原版 `kernel/sched.c:sys_alarm()`。
pub fn alarm(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let seconds = args.a0 as u64;
    crate::pr_warn!("sys_alarm: {} seconds (not implemented)", seconds);
    0
}

/// 获取当前时间。对应原版 `kernel/time.c:sys_gettimeofday()`。
pub fn gettimeofday(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let tv = args.a0 as *mut TimeVal;
    let tz = args.a1 as *mut Timezone;
    if tv.is_null() { return -(EFAULT as i64); }
    unsafe {
        (*tv).tv_sec = 0;
        (*tv).tv_usec = 0;
        if !tz.is_null() {
            (*tz).tz_minuteswest = 0;
            (*tz).tz_dsttime = 0;
        }
    }
    0
}

/// 获取用户 ID。对应原版 `kernel/sys.c:sys_getuid()`。
pub fn getuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 获取有效用户 ID。对应原版 `kernel/sys.c:sys_geteuid()`。
pub fn geteuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 获取组 ID。对应原版 `kernel/sys.c:sys_getgid()`。
pub fn getgid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 获取有效组 ID。对应原版 `kernel/sys.c:sys_getegid()`。
pub fn getegid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置用户 ID。
pub fn setuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置组 ID。
pub fn setgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置进程组。
pub fn setpgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 创建会话。
pub fn setsid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    unsafe { let t = sched::current(); t.session = t.pid; t.pgrp = t.pid; }
    0
}

/// 同步文件系统。
pub fn sync(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 文件同步。
pub fn fsync(args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置文件长度。
pub fn truncate(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置文件长度（ftruncate）。
pub fn ftruncate(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取目录项。
pub fn getdents(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取目录项64。
pub fn getdents64(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 文件描述符控制。
pub fn fchdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取 umask。
pub fn umask(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0o022 }
/// 获取系统信息。
pub fn sysinfo(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let buf = args.a0 as *mut SysInfo;
    if buf.is_null() { return -(EFAULT as i64); }
    unsafe {
        (*buf).uptime = sched::jiffies() as i64;
        (*buf).loads = [0u64; 3];
        (*buf).totalram = 16 * 1024 * 1024;
        (*buf).freeram = 8 * 1024 * 1024;
        (*buf).sharedram = 0; (*buf).bufferram = 0;
        (*buf).totalswap = 0; (*buf).freeswap = 0;
        (*buf).procs = 1;
    }
    0
}
/// 轮询。
pub fn poll(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 多路复用。
pub fn select(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 挂载文件系统。
pub fn mount(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 卸载文件系统。
pub fn umount(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 重新引导。
pub fn reboot(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 资源使用情况。
pub fn getrusage(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let usage = args.a1 as *mut RUsage;
    if !usage.is_null() { unsafe { (*usage).ru_utime.tv_sec = 0; (*usage).ru_utime.tv_usec = 0; (*usage).ru_stime.tv_sec = 0; (*usage).ru_stime.tv_usec = 0; } }
    0
}
/// 资源限制。
pub fn getrlimit(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let rlim = args.a1 as *mut RLimit;
    if !rlim.is_null() { unsafe { (*rlim).rlim_cur = -1i64 as u64; (*rlim).rlim_max = -1i64 as u64; } }
    0
}

/// socket 创建。对应原版 `net/socket.c:sys_socket()`。
pub fn socket(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let domain = args.a0 as i32;
    let socket_type = args.a1 as i32;
    let protocol = args.a2 as i32;
    crate::pr_warn!("sys_socket: domain={}, type={}, protocol={} (stub)", domain, socket_type, protocol);
    -(ENOSYS as i64)
}

/// socket 绑定。
pub fn bind(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// socket 连接。
pub fn connect(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// socket 监听。
pub fn listen(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// socket 接受连接。
pub fn accept(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// socket 发送数据。
pub fn sendto(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// socket 接收数据。
pub fn recvfrom(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// socket 关闭。
pub fn shutdown(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取 socket 名称。
pub fn getsockname(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取 peer 名称。
pub fn getpeername(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置 socket 选项。
pub fn setsockopt(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取 socket 选项。
pub fn getsockopt(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// socket pair。
pub fn socketpair(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// 发送消息。
pub fn sendmsg(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 接收消息。
pub fn recvmsg(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 接收多消息。
pub fn recvmmsg(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 发送多消息。
pub fn sendmmsg(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// clone/fork。对应原版 `kernel/sched.c:sys_fork()`。
pub fn fork(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    crate::pr_warn!("sys_fork: not implemented");
    -(ENOSYS as i64)
}

/// vfork。对应原版 `kernel/sched.c:sys_vfork()`。
pub fn vfork(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    crate::pr_warn!("sys_vfork: not implemented");
    -(ENOSYS as i64)
}

/// wait4 对应原版 `kernel/exit.c:sys_wait4()`。
pub fn wait4(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let pid = args.a0 as i32;
    let status = args.a1 as *mut i32;
    let options = args.a2 as i32;
    crate::pr_warn!("sys_wait4: pid={}, options={} (stub)", pid, options);
    -(ENOSYS as i64)
}

/// 设置定时器。
pub fn setitimer(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取定时器。
pub fn getitimer(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// 原子内存操作。
pub fn mlock(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn munlock(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mlockall(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn munlockall(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mremap(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msync(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// 共享内存。
pub fn shmget(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn shmat(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn shmdt(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn shmctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// 信号量。
pub fn semget(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semop(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semtimedop(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// 消息队列。
pub fn msgget(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgsnd(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgrcv(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// 获取进程优先级。
pub fn getpriority(args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置进程优先级。
pub fn setpriority(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// 设置 hostname。
pub fn sethostname(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置 domainname。
pub fn setdomainname(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// 获取 CPU 信息。
pub fn getcpu(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let cpu = args.a0 as *mut u32;
    let node = args.a1 as *mut u32;
    let tp = args.a2 as *mut u64;
    if !cpu.is_null() { unsafe { *cpu = 0; } }
    if !node.is_null() { unsafe { *node = 0; } }
    if !tp.is_null() { unsafe { *tp = 0; } }
    0
}

/// nanosleep。
pub fn clock_nanosleep(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取时钟时间。
pub fn clock_gettime(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置时钟时间。
pub fn clock_settime(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取时钟分辨率。
pub fn clock_getres(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// prlimit64。
pub fn prlimit(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
