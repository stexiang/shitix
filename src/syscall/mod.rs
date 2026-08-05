//! 系统调用。对应 linux-1.0.9 的 `kernel/sys_call.S` 的分发部分 +
//! `sched.c` 里那张 `sys_call_table[]` + `include/linux/unistd.h` 的调用号。
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`nr`] | `include/linux/unistd.h` 的 `__NR_*` |
//! | [`do_syscall`] | `sys_call.S:_system_call` 里 `call _sys_call_table(,%eax,4)` 那段 |
//! | [`SYS_CALL_TABLE`] | `sched.c` 的 `fn_ptr sys_call_table[]` |
//! | [`sys::*`] | `kernel/sys.c` / `sched.c` 里那些 `sys_*` |
//!
//! **参数传递约定与原版不同**，这是有意的：原版 int 0x80 把调用号放 `eax`、
//! 参数放 `ebx/ecx/edx/esi/edi`。我们保持「调用号在 rax」，但参数改用
//! `rdi/rsi/rdx/r10/r8/r9`——即现代 Linux x86_64 的约定。理由：
//! `syscall` 指令会无条件破坏 `rcx`（存返回地址）和 `r11`（存 rflags），
//! 所以第四个参数必须避开 `rcx` 用 `r10`。沿用这套约定让 `int 0x80` 路径
//! 和将来的 `syscall` 指令路径能共用同一个 [`do_syscall`]。
//!
//! 原版 `sys_call_table` 有 137 项，绝大多数依赖 `fs/`（read/write/open/…）、
//! `kernel/signal.c`、`mm/mmap.c`。这里只实现不依赖它们的那些，其余全部指向
//! [`sys::ni_syscall`]（原版 `sys_ni_syscall`，返回 `-EINVAL`）。

pub mod sys;

use crate::klib::errno::ENOSYS;
use crate::klib::printk::Level;
use crate::traps::PtRegs;

/// 系统调用号。x86_64 Linux 系统调用号
pub mod nr {
    // 进程管理
    pub const EXIT: usize = 60;
    pub const EXIT_GROUP: usize = 231;
    pub const FORK: usize = 57;
    pub const VFORK: usize = 58;
    pub const EXECVE: usize = 59;
    pub const WAIT4: usize = 61;
    pub const WAITPID: usize = 7;
    pub const KILL: usize = 62;
    pub const UNAME: usize = 160;
    pub const GETPID: usize = 61;
    pub const GETPPID: usize = 62;
    pub const GETPGRP: usize = 76;
    pub const GETPGID: usize = 121;
    pub const SETPGID: usize = 123;
    pub const SETSID: usize = 66;
    
    // 文件操作
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const OPEN: usize = 2;
    pub const CLOSE: usize = 3;
    pub const STAT: usize = 4;
    pub const FSTAT: usize = 5;
    pub const LSTAT: usize = 6;
    pub const PIVOT_ROOT: usize = 155;
    pub const GETCWD: usize = 17;
    pub const CHDIR: usize = 80;
    pub const FCHDIR: usize = 81;
    pub const RENAME: usize = 82;
    pub const MKDIR: usize = 83;
    pub const RMDIR: usize = 84;
    pub const CREAT: usize = 85;
    pub const LINK: usize = 86;
    pub const UNLINK: usize = 87;
    pub const SYMLINK: usize = 88;
    pub const READLINK: usize = 89;
    pub const CHMOD: usize = 90;
    pub const FCHMOD: usize = 91;
    pub const CHOWN: usize = 92;
    pub const FCHOWN: usize = 93;
    pub const LCHOWN: usize = 94;
    pub const UMASK: usize = 95;
    pub const GETTIMEOFDAY: usize = 96;
    pub const GETRLIMIT: usize = 97;
    pub const GETRUSAGE: usize = 98;
    pub const SYSINFO: usize = 99;
    pub const TIMES: usize = 100;
    pub const GETUID: usize = 102;
    pub const SYSLOG: usize = 103;
    pub const GETGID: usize = 104;
    pub const SETUID: usize = 105;
    pub const SETGID: usize = 106;
    pub const GETEUID: usize = 107;
    pub const GETEGID: usize = 108;
    
    // 内存管理
    pub const BRK: usize = 12;
    pub const MMAP: usize = 9;
    pub const MUNMAP: usize = 11;
    pub const MPROTECT: usize = 10;
    pub const MLOCK: usize = 224;
    pub const MUNLOCK: usize = 225;
    pub const MLOCKALL: usize = 226;
    pub const MUNLOCKALL: usize = 227;
    pub const MREMAP: usize = 25;
    pub const MSYNC: usize = 26;
    pub const MINCORE: usize = 27;
    pub const MADVISE: usize = 28;
    pub const MMAP2: usize = 9;
    
    // 文件描述符
    pub const DUP: usize = 32;
    pub const DUP2: usize = 33;
    pub const PAUSE: usize = 34;
    pub const SELECT: usize = 23;
    pub const POLL: usize = 7;
    pub const EPOLL_CREATE: usize = 13;
    pub const EPOLL_CTL: usize = 14;
    pub const EPOLL_WAIT: usize = 15;
    pub const PIPE: usize = 22;
    pub const PIPE2: usize = 293;
    pub const SETITIMER: usize = 38;
    pub const GETITIMER: usize = 39;
    pub const SETHOSTNAME: usize = 147;
    pub const SETDOMAINNAME: usize = 146;
    pub const IOPERM: usize = 173;
    pub const IOPL: usize = 172;
    pub const INIT_MODULE: usize = 175;
    pub const DELETE_MODULE: usize = 176;
    pub const FCNTL: usize = 72;
    pub const FLOCK: usize = 73;
    pub const FSYNC: usize = 74;
    pub const FDATASYNC: usize = 75;
    
    // 信号
    pub const ALARM: usize = 37;
    pub const SIGNAL: usize = 48;
    pub const RT_SIGACTION: usize = 13;
    pub const RT_SIGPROCMASK: usize = 14;
    pub const RT_SIGRETURN: usize = 15;
    pub const RT_SIGSUSPEND: usize = 24;
    
    // 时间
    pub const CLOCK_GETRES: usize = 229;
    pub const CLOCK_GETTIME: usize = 228;
    pub const CLOCK_SETTIME: usize = 227;
    pub const CLOCK_NANOSLEEP: usize = 230;
    pub const TIMER_CREATE: usize = 222;
    pub const TIMER_SETTIME: usize = 223;
    pub const TIMER_GETTIME: usize = 224;
    pub const TIMER_GETOVERRUN: usize = 225;
    pub const TIMER_DELETE: usize = 226;
    
    // 挂载
    pub const MOUNT: usize = 165;
    pub const UMOUNT: usize = 166;
    pub const UMOUNT2: usize = 166;
    
    // 其他系统调用
    pub const READV: usize = 19;
    pub const WRITEV: usize = 20;
    pub const ACCESS: usize = 21;
    pub const PREAD64: usize = 17;
    pub const PWRITE64: usize = 18;
    pub const TRUNCATE: usize = 76;
    pub const FTRUNCATE: usize = 77;
    pub const GETDENTS: usize = 78;
    pub const GETDENTS64: usize = 61;
    pub const FUTIMESAT: usize = 261;
    pub const FUTIMENSAT: usize = 262;
    pub const READLINKAT: usize = 267;
    pub const SYMLINKAT: usize = 266;
    pub const LINKAT: usize = 265;
    pub const UNLINKAT: usize = 263;
    pub const MKDIRAT: usize = 258;
    pub const MKNODAT: usize = 259;
    pub const RENAMEAT: usize = 264;
    pub const MKNOD: usize = 133;
    pub const ACCT: usize = 163;
    pub const SWAPON: usize = 167;
    pub const SWAPOFF: usize = 168;
    pub const REBOOT: usize = 169;
    pub const SETRESUID: usize = 147;
    pub const GETRESUID: usize = 148;
    pub const SETRESGID: usize = 149;
    pub const GETRESGID: usize = 150;
    pub const SETFSUID: usize = 151;
    pub const SETFSGID: usize = 152;
    pub const SETREUID: usize = 117;
    pub const SETREGID: usize = 119;
    pub const GETGROUPS: usize = 115;
    pub const SETGROUPS: usize = 116;
    pub const SETPRIORITY: usize = 141;
    pub const GETPRIORITY: usize = 142;
    
    // 内存策略
    pub const MBIND: usize = 237;
    pub const GETMEMPOLICY: usize = 238;
    pub const SETMEMPOLICY: usize = 239;
    
    // IPC
    pub const MSGSND: usize = 69;
    pub const MSGRCV: usize = 70;
    pub const MSGGET: usize = 68;
    pub const MSGCTL: usize = 71;
    pub const SEMGET: usize = 64;
    pub const SEMOP: usize = 65;
    pub const SEMCTL: usize = 66;
    pub const SHMGET: usize = 73;
    pub const SHMCTL: usize = 74;
    pub const SHMAT: usize = 72;
    pub const SHMDT: usize = 75;
    
    // 网络
    pub const SOCKET: usize = 41;
    pub const SOCKETPAIR: usize = 53;
    pub const BIND: usize = 49;
    pub const LISTEN: usize = 50;
    pub const ACCEPT: usize = 43;
    pub const CONNECT: usize = 42;
    pub const GETSOCKNAME: usize = 51;
    pub const GETPEERNAME: usize = 52;
    pub const SENDTO: usize = 44;
    pub const RECVFROM: usize = 45;
    pub const SHUTDOWN: usize = 48;
    pub const SETSOCKOPT: usize = 54;
    pub const GETSOCKOPT: usize = 55;
    pub const SENDMSG: usize = 46;
    pub const RECVMSG: usize = 47;
    
    // 扩展
    pub const PRLIMIT: usize = 134;
    pub const RECVMMSG: usize = 299;
    pub const SENDMMSG: usize = 307;
    pub const SETNS: usize = 308;
    pub const GETCPU: usize = 309;
    pub const SEMTIMEDOP: usize = 67;
    pub const CAPGET: usize = 90;
    pub const CAPSET: usize = 91;
    pub const PTRACE: usize = 101;
    
    // IDLE
    pub const IDLE: usize = 112;
    
    /// 表的容量
    pub const NR_SYSCALLS: usize = 512;
}

/// 系统调用处理函数签名。
///
/// 原版是 `typedef int (*fn_ptr)()`——一个无原型的函数指针，靠 C 的
/// 老式调用约定让每个 `sys_*` 自己声明参数个数（`sys_getpid(void)` 和
/// `sys_write(int,const char*,off_t)` 塞进同一个数组）。Rust 不允许这种
/// 类型双关，所以统一成「收全部 6 个参数 + pt_regs」，各实现忽略不用的。
pub type SysFn = fn(args: &SysArgs, regs: &mut PtRegs) -> i64;

/// 系统调用的六个参数。字段名对应寄存器，取值来自 [`PtRegs`]。
pub struct SysArgs {
    pub a0: u64,
    pub a1: u64,
    pub a2: u64,
    pub a3: u64,
    pub a4: u64,
    pub a5: u64,
}

impl SysArgs {
    /// 按 x86_64 约定从 pt_regs 里取参数（见模块文档说明为何用 r10 而非 rcx）。
    fn from_regs(regs: &PtRegs) -> Self {
        SysArgs {
            a0: regs.rdi,
            a1: regs.rsi,
            a2: regs.rdx,
            a3: regs.r10,
            a4: regs.r8,
            a5: regs.r9,
        }
    }
}

/// 分发表。对应原版 `sched.c` 里那张 `sys_call_table[]`。
///
/// 原版把 137 个函数名一字排开；我们用「默认 ni_syscall + 显式覆盖」的方式
/// 建表，因为绝大多数项现在还是空的，一字排开只会是 130 行 `ni_syscall`。
static SYS_CALL_TABLE: [SysFn; nr::NR_SYSCALLS] = {
    let mut t: [SysFn; nr::NR_SYSCALLS] = [sys::ni_syscall; nr::NR_SYSCALLS];
    t[nr::EXIT] = sys::exit;
    t[nr::GETPID] = sys::getpid;
    t[nr::GETPPID] = sys::getppid;
    t[nr::GETPGRP] = sys::getpgrp;
    t[nr::PAUSE] = sys::pause;
    t[nr::TIMES] = sys::times;
    t[nr::WRITE] = sys::write;
    t[nr::UNAME] = sys::uname;
    t[nr::IDLE] = sys::idle;
    // LFS Critical Syscalls
    t[nr::READ] = sys::read;
    t[nr::OPEN] = sys::open;
    t[nr::CLOSE] = sys::close;
    t[nr::BRK] = sys::brk;
    t[nr::MMAP] = sys::mmap;
    t[nr::MUNMAP] = sys::munmap;
    t[nr::MPROTECT] = sys::mprotect;
    t[nr::CREAT] = sys::creat;
    t[nr::STAT] = sys::stat;
    t[nr::FSTAT] = sys::fstat;
    t[nr::LSTAT] = sys::lstat;
    t[nr::CHDIR] = sys::chdir;
    t[nr::MKDIR] = sys::mkdir;
    t[nr::RMDIR] = sys::rmdir;
    t[nr::UNLINK] = sys::unlink;
    t[nr::SYMLINK] = sys::symlink;
    t[nr::READLINK] = sys::readlink;
    t[nr::CHMOD] = sys::chmod;
    t[nr::CHOWN] = sys::chown;
    t[nr::DUP] = sys::dup;
    t[nr::DUP2] = sys::dup2;
    t[nr::GETCWD] = sys::getcwd;
    t[nr::RENAME] = sys::rename;
    t[nr::MKNOD] = sys::mknod;
    t
};

/// 已处理的系统调用总数。自检用；原版没有（`kstat` 不统计这个）。
static mut SYSCALL_COUNT: u64 = 0;

/// 系统调用总数。
pub fn syscall_count() -> u64 {
    // SAFETY: 只读一个 u64。
    unsafe { *core::ptr::addr_of!(SYSCALL_COUNT) }
}

/// 系统调用的统一入口，由 `boot/entry.S` 的 `system_call` 调用。
///
/// 对应原版 `_system_call` 里从 `cmpl _NR_syscalls,%eax` 到
/// `movl %eax,EAX(%esp)` 那一段。原版的三件事全部保留：
/// 1. 调用号越界 → 返回 `-ENOSYS`（原版 `movl $-ENOSYS,EAX(%esp)`）
/// 2. 调用前清 `current->errno`
/// 3. 返回后若 `errno` 非 0，用 `-errno` 覆盖返回值
///
/// 原版还会设 / 清 CF 标志来指示错误（`orl $CF_MASK,EFLAGS(%esp)`），
/// 那是给 libc 的 `_syscall` 宏用的老式约定。现代约定是「返回值本身为
/// 负 errno」，两者都实现了：CF 照原版设，返回值也是负 errno。
///
/// # Safety
/// 只能由 entry.S 的 `system_call` 桩调用，`regs` 必须指向内核栈上
/// 刚由 SAVE_ALL 建好的完整 `PtRegs`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn do_syscall(regs: *mut PtRegs) {
    // SAFETY: 契约保证 regs 有效且我们独占。
    let regs = unsafe { &mut *regs };
    // 调用号在 orig_rax（entry.S 的 `pushq %rax` 存的），同原版 orig_eax
    let call_nr = regs.orig_rax as usize;

    // SAFETY: 单核；系统调用不可重入到自身。
    unsafe { *core::ptr::addr_of_mut!(SYSCALL_COUNT) += 1 }

    // 原版：`cmpl _NR_syscalls,%eax; jae ret_from_sys_call`（此前已把
    // EAX 格预置成 -ENOSYS）
    if call_nr >= nr::NR_SYSCALLS {
        regs.rax = (-(ENOSYS as i64)) as u64;
        set_carry(regs, true);
        return;
    }

    // 原版：`movl $0,errno(%ebx)`
    // SAFETY: 系统调用只可能来自某个存活任务，current 有效。
    let cur = unsafe { crate::sched::current() };
    cur.errno = 0;

    let args = SysArgs::from_regs(regs);
    let ret = SYS_CALL_TABLE[call_nr](&args, regs);

    // 原版：先写返回值，再看 errno 是否要覆盖
    //   movl %eax,EAX(%esp); movl errno(%ebx),%edx; negl %edx; je ret_...
    // SAFETY: 同上，current 在整个系统调用期间有效。
    let errno = unsafe { crate::sched::current() }.errno;
    if errno != 0 {
        regs.rax = (-(errno as i64)) as u64;
        set_carry(regs, true);
    } else {
        regs.rax = ret as u64;
        // 负返回值也算错误（现代约定），CF 照原版一起设
        set_carry(regs, ret < 0);
    }
}

/// 设置/清除返回给用户态的 CF 标志。
/// 对应原版 `orl $(CF_MASK),EFLAGS(%esp)` / `andl $~CF_MASK,EFLAGS(%esp)`。
fn set_carry(regs: &mut PtRegs, carry: bool) {
    if carry {
        regs.rflags |= 1;
    } else {
        regs.rflags &= !1;
    }
}

/// 从内核里直接发起一次系统调用，走真正的 `int 0x80` 路径。
///
/// 原版没有这个（内核里要用某个功能就直接调 `sys_xxx`，比如 `init()` 里
/// 那堆 `_syscall0` 内联宏其实是从**内核态**执行 `int 0x80`）。这里显式
/// 提供，用来在没有用户态进程的情况下验证整条陷入/返回链路。
///
/// # Safety
/// 必须在 IDT 装好（`int 0x80` 的门存在）之后调用。
/// 从内核态执行 `int 0x80` 不会换栈（CPL 不变），所以当前内核栈上要有
/// 足够空间放一份 pt_regs（约 0xa8 字节）。
pub unsafe fn syscall3(number: usize, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: 契约保证 IDT 就绪。int 0x80 的门是 DPL=3 的陷阱门，
    // 从 CPL=0 触发同样合法。clobber 列表覆盖 entry.S 里 SAVE_ALL 之外
    // 可能被破坏的寄存器；rax 是返回值。
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") number as u64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            // entry.S 的 SAVE_ALL/RESTORE_ALL 会完整恢复所有通用寄存器，
            // 所以除了 rax（返回值）之外无需声明 clobber。不能加
            // preserves_flags：do_syscall 会按约定改写返回的 CF。
            // 也不能加 nomem：系统调用可以改内存。
        );
    }
    ret
}

/// 无参数版本。
///
/// # Safety
/// 同 [`syscall3`]。
pub unsafe fn syscall0(number: usize) -> i64 {
    // SAFETY: 契约转交。
    unsafe { syscall3(number, 0, 0, 0) }
}

/// 打印系统调用统计，供启动自检。
pub fn dump() {
    crate::pr!(Level::Info, "syscall: table={} entries, {} calls served",
               nr::NR_SYSCALLS, syscall_count());
}
