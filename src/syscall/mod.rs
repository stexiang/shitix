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

/// 系统调用号。**x86_64 Linux 的正式调用号**（`arch/x86/entry/syscalls/
/// syscall_64.tbl`），不是 linux-1.0.9 的 i386 号。
///
/// `0..=334` 连续排满，然后是 `424..=448`——**`335..=423` 在官方 x86_64 表里
/// 本来就是空的**（留给 x32 ABI），不是这里漏了。全部 360 个号与
/// `unistd_64.h`（5.15）逐项核对一致，核对脚本见 STATUS 的验证记录。
/// 早期版本这里按功能分组手写，出现了 `GETPID`/`WAIT4` 都等于 61 这类重号，
/// 建表时后写的项会静默覆盖前一项。现在改成从正式表逐号生成，杜绝重号。
#[allow(dead_code)]
pub mod nr {
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const OPEN: usize = 2;
    pub const CLOSE: usize = 3;
    pub const STAT: usize = 4;
    pub const FSTAT: usize = 5;
    pub const LSTAT: usize = 6;
    pub const POLL: usize = 7;
    pub const LSEEK: usize = 8;
    pub const MMAP: usize = 9;
    pub const MPROTECT: usize = 10;
    pub const MUNMAP: usize = 11;
    pub const BRK: usize = 12;
    pub const RT_SIGACTION: usize = 13;
    pub const RT_SIGPROCMASK: usize = 14;
    pub const RT_SIGRETURN: usize = 15;
    pub const IOCTL: usize = 16;
    pub const PREAD64: usize = 17;
    pub const PWRITE64: usize = 18;
    pub const READV: usize = 19;
    pub const WRITEV: usize = 20;
    pub const ACCESS: usize = 21;
    pub const PIPE: usize = 22;
    pub const SELECT: usize = 23;
    pub const SCHED_YIELD: usize = 24;
    pub const MREMAP: usize = 25;
    pub const MSYNC: usize = 26;
    pub const MINCORE: usize = 27;
    pub const MADVISE: usize = 28;
    pub const SHMGET: usize = 29;
    pub const SHMAT: usize = 30;
    pub const SHMCTL: usize = 31;
    pub const DUP: usize = 32;
    pub const DUP2: usize = 33;
    pub const PAUSE: usize = 34;
    pub const NANOSLEEP: usize = 35;
    pub const GETITIMER: usize = 36;
    pub const ALARM: usize = 37;
    pub const SETITIMER: usize = 38;
    pub const GETPID: usize = 39;
    pub const SENDFILE: usize = 40;
    pub const SOCKET: usize = 41;
    pub const CONNECT: usize = 42;
    pub const ACCEPT: usize = 43;
    pub const SENDTO: usize = 44;
    pub const RECVFROM: usize = 45;
    pub const SENDMSG: usize = 46;
    pub const RECVMSG: usize = 47;
    pub const SHUTDOWN: usize = 48;
    pub const BIND: usize = 49;
    pub const LISTEN: usize = 50;
    pub const GETSOCKNAME: usize = 51;
    pub const GETPEERNAME: usize = 52;
    pub const SOCKETPAIR: usize = 53;
    pub const SETSOCKOPT: usize = 54;
    pub const GETSOCKOPT: usize = 55;
    pub const CLONE: usize = 56;
    pub const FORK: usize = 57;
    pub const VFORK: usize = 58;
    pub const EXECVE: usize = 59;
    pub const EXIT: usize = 60;
    pub const WAIT4: usize = 61;
    pub const KILL: usize = 62;
    pub const UNAME: usize = 63;
    pub const SEMGET: usize = 64;
    pub const SEMOP: usize = 65;
    pub const SEMCTL: usize = 66;
    pub const SHMDT: usize = 67;
    pub const MSGGET: usize = 68;
    pub const MSGSND: usize = 69;
    pub const MSGRCV: usize = 70;
    pub const MSGCTL: usize = 71;
    pub const FCNTL: usize = 72;
    pub const FLOCK: usize = 73;
    pub const FSYNC: usize = 74;
    pub const FDATASYNC: usize = 75;
    pub const TRUNCATE: usize = 76;
    pub const FTRUNCATE: usize = 77;
    pub const GETDENTS: usize = 78;
    pub const GETCWD: usize = 79;
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
    pub const PTRACE: usize = 101;
    pub const GETUID: usize = 102;
    pub const SYSLOG: usize = 103;
    pub const GETGID: usize = 104;
    pub const SETUID: usize = 105;
    pub const SETGID: usize = 106;
    pub const GETEUID: usize = 107;
    pub const GETEGID: usize = 108;
    pub const SETPGID: usize = 109;
    pub const GETPPID: usize = 110;
    pub const GETPGRP: usize = 111;
    pub const SETSID: usize = 112;
    pub const SETREUID: usize = 113;
    pub const SETREGID: usize = 114;
    pub const GETGROUPS: usize = 115;
    pub const SETGROUPS: usize = 116;
    pub const SETRESUID: usize = 117;
    pub const GETRESUID: usize = 118;
    pub const SETRESGID: usize = 119;
    pub const GETRESGID: usize = 120;
    pub const GETPGID: usize = 121;
    pub const SETFSUID: usize = 122;
    pub const SETFSGID: usize = 123;
    pub const GETSID: usize = 124;
    pub const CAPGET: usize = 125;
    pub const CAPSET: usize = 126;
    pub const RT_SIGPENDING: usize = 127;
    pub const RT_SIGTIMEDWAIT: usize = 128;
    pub const RT_SIGQUEUEINFO: usize = 129;
    pub const RT_SIGSUSPEND: usize = 130;
    pub const SIGALTSTACK: usize = 131;
    pub const UTIME: usize = 132;
    pub const MKNOD: usize = 133;
    pub const USELIB: usize = 134;
    pub const PERSONALITY: usize = 135;
    pub const USTAT: usize = 136;
    pub const STATFS: usize = 137;
    pub const FSTATFS: usize = 138;
    pub const SYSFS: usize = 139;
    pub const GETPRIORITY: usize = 140;
    pub const SETPRIORITY: usize = 141;
    pub const SCHED_SETPARAM: usize = 142;
    pub const SCHED_GETPARAM: usize = 143;
    pub const SCHED_SETSCHEDULER: usize = 144;
    pub const SCHED_GETSCHEDULER: usize = 145;
    pub const SCHED_GET_PRIORITY_MAX: usize = 146;
    pub const SCHED_GET_PRIORITY_MIN: usize = 147;
    pub const SCHED_RR_GET_INTERVAL: usize = 148;
    pub const MLOCK: usize = 149;
    pub const MUNLOCK: usize = 150;
    pub const MLOCKALL: usize = 151;
    pub const MUNLOCKALL: usize = 152;
    pub const VHANGUP: usize = 153;
    pub const MODIFY_LDT: usize = 154;
    pub const PIVOT_ROOT: usize = 155;
    /// 官方表里拼作 `_sysctl`（早已废弃并在 5.5 后返回 -ENOSYS）。
    /// Rust 侧去掉前导下划线以免被当成「有意未使用」。号一致。
    pub const SYSCTL: usize = 156;
    pub const PRCTL: usize = 157;
    pub const ARCH_PRCTL: usize = 158;
    pub const ADJTIMEX: usize = 159;
    pub const SETRLIMIT: usize = 160;
    pub const CHROOT: usize = 161;
    pub const SYNC: usize = 162;
    pub const ACCT: usize = 163;
    pub const SETTIMEOFDAY: usize = 164;
    pub const MOUNT: usize = 165;
    pub const UMOUNT2: usize = 166;
    pub const SWAPON: usize = 167;
    pub const SWAPOFF: usize = 168;
    pub const REBOOT: usize = 169;
    pub const SETHOSTNAME: usize = 170;
    pub const SETDOMAINNAME: usize = 171;
    pub const IOPL: usize = 172;
    pub const IOPERM: usize = 173;
    pub const CREATE_MODULE: usize = 174;
    pub const INIT_MODULE: usize = 175;
    pub const DELETE_MODULE: usize = 176;
    pub const GET_KERNEL_SYMS: usize = 177;
    pub const QUERY_MODULE: usize = 178;
    pub const QUOTACTL: usize = 179;
    pub const NFSSERVCTL: usize = 180;
    pub const GETPMSG: usize = 181;
    pub const PUTPMSG: usize = 182;
    pub const AFS_SYSCALL: usize = 183;
    pub const TUXCALL: usize = 184;
    pub const SECURITY: usize = 185;
    pub const GETTID: usize = 186;
    pub const READAHEAD: usize = 187;
    pub const SETXATTR: usize = 188;
    pub const LSETXATTR: usize = 189;
    pub const FSETXATTR: usize = 190;
    pub const GETXATTR: usize = 191;
    pub const LGETXATTR: usize = 192;
    pub const FGETXATTR: usize = 193;
    pub const LISTXATTR: usize = 194;
    pub const LLISTXATTR: usize = 195;
    pub const FLISTXATTR: usize = 196;
    pub const REMOVEXATTR: usize = 197;
    pub const LREMOVEXATTR: usize = 198;
    pub const FREMOVEXATTR: usize = 199;
    pub const TKILL: usize = 200;
    pub const TIME: usize = 201;
    pub const FUTEX: usize = 202;
    pub const SCHED_SETAFFINITY: usize = 203;
    pub const SCHED_GETAFFINITY: usize = 204;
    pub const SET_THREAD_AREA: usize = 205;
    pub const IO_SETUP: usize = 206;
    pub const IO_DESTROY: usize = 207;
    pub const IO_GETEVENTS: usize = 208;
    pub const IO_SUBMIT: usize = 209;
    pub const IO_CANCEL: usize = 210;
    pub const GET_THREAD_AREA: usize = 211;
    pub const LOOKUP_DCOOKIE: usize = 212;
    pub const EPOLL_CREATE: usize = 213;
    pub const EPOLL_CTL_OLD: usize = 214;
    pub const EPOLL_WAIT_OLD: usize = 215;
    pub const REMAP_FILE_PAGES: usize = 216;
    pub const GETDENTS64: usize = 217;
    pub const SET_TID_ADDRESS: usize = 218;
    pub const RESTART_SYSCALL: usize = 219;
    pub const SEMTIMEDOP: usize = 220;
    pub const FADVISE64: usize = 221;
    pub const TIMER_CREATE: usize = 222;
    pub const TIMER_SETTIME: usize = 223;
    pub const TIMER_GETTIME: usize = 224;
    pub const TIMER_GETOVERRUN: usize = 225;
    pub const TIMER_DELETE: usize = 226;
    pub const CLOCK_SETTIME: usize = 227;
    pub const CLOCK_GETTIME: usize = 228;
    pub const CLOCK_GETRES: usize = 229;
    pub const CLOCK_NANOSLEEP: usize = 230;
    pub const EXIT_GROUP: usize = 231;
    pub const EPOLL_WAIT: usize = 232;
    pub const EPOLL_CTL: usize = 233;
    pub const TGKILL: usize = 234;
    pub const UTIMES: usize = 235;
    pub const VSERVER: usize = 236;
    pub const MBIND: usize = 237;
    pub const SET_MEMPOLICY: usize = 238;
    pub const GET_MEMPOLICY: usize = 239;
    pub const MQ_OPEN: usize = 240;
    pub const MQ_UNLINK: usize = 241;
    pub const MQ_TIMEDSEND: usize = 242;
    pub const MQ_TIMEDRECEIVE: usize = 243;
    pub const MQ_NOTIFY: usize = 244;
    pub const MQ_GETSETATTR: usize = 245;
    pub const KEXEC_LOAD: usize = 246;
    pub const WAITID: usize = 247;
    pub const ADD_KEY: usize = 248;
    pub const REQUEST_KEY: usize = 249;
    pub const KEYCTL: usize = 250;
    pub const IOPRIO_SET: usize = 251;
    pub const IOPRIO_GET: usize = 252;
    pub const INOTIFY_INIT: usize = 253;
    pub const INOTIFY_ADD_WATCH: usize = 254;
    pub const INOTIFY_RM_WATCH: usize = 255;
    pub const MIGRATE_PAGES: usize = 256;
    pub const OPENAT: usize = 257;
    pub const MKDIRAT: usize = 258;
    pub const MKNODAT: usize = 259;
    pub const FCHOWNAT: usize = 260;
    pub const FUTIMESAT: usize = 261;
    pub const NEWFSTATAT: usize = 262;
    pub const UNLINKAT: usize = 263;
    pub const RENAMEAT: usize = 264;
    pub const LINKAT: usize = 265;
    pub const SYMLINKAT: usize = 266;
    pub const READLINKAT: usize = 267;
    pub const FCHMODAT: usize = 268;
    pub const FACCESSAT: usize = 269;
    pub const PSELECT6: usize = 270;
    pub const PPOLL: usize = 271;
    pub const UNSHARE: usize = 272;
    pub const SET_ROBUST_LIST: usize = 273;
    pub const GET_ROBUST_LIST: usize = 274;
    pub const SPLICE: usize = 275;
    pub const TEE: usize = 276;
    pub const SYNC_FILE_RANGE: usize = 277;
    pub const VMSPLICE: usize = 278;
    pub const MOVE_PAGES: usize = 279;
    pub const UTIMENSAT: usize = 280;
    pub const EPOLL_PWAIT: usize = 281;
    pub const SIGNALFD: usize = 282;
    pub const TIMERFD_CREATE: usize = 283;
    pub const EVENTFD: usize = 284;
    pub const FALLOCATE: usize = 285;
    pub const TIMERFD_SETTIME: usize = 286;
    pub const TIMERFD_GETTIME: usize = 287;
    pub const ACCEPT4: usize = 288;
    pub const SIGNALFD4: usize = 289;
    pub const EVENTFD2: usize = 290;
    pub const EPOLL_CREATE1: usize = 291;
    pub const DUP3: usize = 292;
    pub const PIPE2: usize = 293;
    pub const INOTIFY_INIT1: usize = 294;
    pub const PREADV: usize = 295;
    pub const PWRITEV: usize = 296;
    pub const RT_TGSIGQUEUEINFO: usize = 297;
    pub const PERF_EVENT_OPEN: usize = 298;
    pub const RECVMMSG: usize = 299;
    pub const FANOTIFY_INIT: usize = 300;
    pub const FANOTIFY_MARK: usize = 301;
    pub const PRLIMIT64: usize = 302;
    pub const NAME_TO_HANDLE_AT: usize = 303;
    pub const OPEN_BY_HANDLE_AT: usize = 304;
    pub const CLOCK_ADJTIME: usize = 305;
    pub const SYNCFS: usize = 306;
    pub const SENDMMSG: usize = 307;
    pub const SETNS: usize = 308;
    pub const GETCPU: usize = 309;
    pub const PROCESS_VM_READV: usize = 310;
    pub const PROCESS_VM_WRITEV: usize = 311;
    pub const KCMP: usize = 312;
    pub const FINIT_MODULE: usize = 313;
    pub const SCHED_SETATTR: usize = 314;
    pub const SCHED_GETATTR: usize = 315;
    pub const RENAMEAT2: usize = 316;
    pub const SECCOMP: usize = 317;
    pub const GETRANDOM: usize = 318;
    pub const MEMFD_CREATE: usize = 319;
    pub const KEXEC_FILE_LOAD: usize = 320;
    pub const BPF: usize = 321;
    pub const EXECVEAT: usize = 322;
    pub const USERFAULTFD: usize = 323;
    pub const MEMBARRIER: usize = 324;
    pub const MLOCK2: usize = 325;
    pub const COPY_FILE_RANGE: usize = 326;
    pub const PREADV2: usize = 327;
    pub const PWRITEV2: usize = 328;
    pub const PKEY_MPROTECT: usize = 329;
    pub const PKEY_ALLOC: usize = 330;
    pub const PKEY_FREE: usize = 331;
    pub const STATX: usize = 332;
    pub const IO_PGETEVENTS: usize = 333;
    pub const RSEQ: usize = 334;
    pub const PIDFD_SEND_SIGNAL: usize = 424;
    pub const OPEN_TREE: usize = 428;
    pub const MOVE_MOUNT: usize = 429;
    pub const FSOPEN: usize = 430;
    pub const FSCONFIG: usize = 431;
    pub const FSMOUNT: usize = 432;
    pub const FSPICK: usize = 433;
    pub const CLOSE_RANGE: usize = 436;
    pub const OPENAT2: usize = 437;
    pub const PIDFD_GETFD: usize = 438;
    pub const PROCESS_MADVISE: usize = 440;
    pub const MOUNT_SETATTR: usize = 442;
    pub const QUOTACTL_FD: usize = 443;
    pub const LANDLOCK_CREATE_RULESET: usize = 444;
    pub const LANDLOCK_ADD_RULE: usize = 445;
    pub const LANDLOCK_RESTRICT_SELF: usize = 446;
    pub const MEMFD_SECRET: usize = 447;
    pub const PROCESS_MRELEASE: usize = 448;
    pub const IO_URING_SETUP: usize = 425;
    pub const IO_URING_ENTER: usize = 426;
    pub const IO_URING_REGISTER: usize = 427;
    pub const PIDFD_OPEN: usize = 434;
    pub const CLONE3: usize = 435;
    pub const FACCESSAT2: usize = 439;
    pub const EPOLL_PWAIT2: usize = 441;

    /// 兼容别名：本树里 `sys_umount` 实现的是 `umount2` 语义。
    pub const UMOUNT: usize = UMOUNT2;
    /// 兼容别名：`prlimit64` 的旧名。
    pub const PRLIMIT: usize = PRLIMIT64;
    /// 兼容别名：老代码里 `SETMEMPOLICY` 的写法。
    pub const SETMEMPOLICY: usize = SET_MEMPOLICY;
    pub const GETMEMPOLICY: usize = GET_MEMPOLICY;
    /// 原版 1.0.9 的 `sys_idle`（i386 号 112）。x86_64 表里 112 是 `setsid`，
    /// 所以自检用的 idle 挪到表尾的私有号段。
    pub const IDLE: usize = 500;

    /// 一个保证落在表内、且保证是 `ni_syscall` 的号。给自检用，
    /// 用来区分「号越界 → -ENOSYS」和「号合法但未实现 → -EINVAL」两条路径。
    pub const UNUSED: usize = 501;

    /// 表的容量。
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
/// 按调用号顺序逐项赋值（号从 [`nr`] 取，不写字面量），未实现的保持
/// [`sys::ni_syscall`]。行尾注释是号，方便和正式 `syscall_64.tbl` 对照。
static mut SYS_CALL_TABLE: [Option<SysFn>; nr::NR_SYSCALLS] = [None; nr::NR_SYSCALLS];

/// 分发表运行时填充（all-None 落 BSS，省 4KB .data；0x90000 安全区）。
/// 必须在首个系统调用可达之前调用。
pub unsafe fn init_table() {
    unsafe {
        let t = &mut *core::ptr::addr_of_mut!(SYS_CALL_TABLE);
        t[nr::READ] = Some(sys::read); // 0
        t[nr::WRITE] = Some(sys::write); // 1
        t[nr::OPEN] = Some(sys::open); // 2
        t[nr::CLOSE] = Some(sys::close); // 3
        t[nr::STAT] = Some(sys::stat); // 4
        t[nr::FSTAT] = Some(sys::fstat); // 5
        t[nr::LSTAT] = Some(sys::lstat); // 6
        t[nr::POLL] = Some(sys::poll); // 7
        t[nr::LSEEK] = Some(sys::lseek); // 8
        t[nr::MMAP] = Some(sys::mmap); // 9
        t[nr::MPROTECT] = Some(sys::mprotect); // 10
        t[nr::MUNMAP] = Some(sys::munmap); // 11
        t[nr::BRK] = Some(sys::brk); // 12
        t[nr::RT_SIGACTION] = Some(sys::rt_sigaction); // 13
        t[nr::RT_SIGPROCMASK] = Some(sys::rt_sigprocmask); // 14
        t[nr::RT_SIGRETURN] = Some(sys::rt_sigreturn); // 15
        t[nr::IOCTL] = Some(sys::ioctl); // 16
        t[nr::PREAD64] = Some(sys::pread64); // 17
        t[nr::PWRITE64] = Some(sys::pwrite64); // 18
        t[nr::READV] = Some(sys::readv); // 19
        t[nr::WRITEV] = Some(sys::writev); // 20
        t[nr::ACCESS] = Some(sys::access); // 21
        t[nr::PIPE] = Some(sys::pipe); // 22
        t[nr::SELECT] = Some(sys::select); // 23
        t[nr::SCHED_YIELD] = Some(sys::sched_yield); // 24
        t[nr::MREMAP] = Some(sys::mremap); // 25
        t[nr::MSYNC] = Some(sys::msync); // 26
        t[nr::MINCORE] = Some(sys::mincore); // 27
        t[nr::MADVISE] = Some(sys::madvise); // 28
        t[nr::SHMGET] = Some(sys::shmget); // 29
        t[nr::SHMAT] = Some(sys::shmat); // 30
        t[nr::SHMCTL] = Some(sys::shmctl); // 31
        t[nr::DUP] = Some(sys::dup); // 32
        t[nr::DUP2] = Some(sys::dup2); // 33
        t[nr::PAUSE] = Some(sys::pause); // 34
        t[nr::NANOSLEEP] = Some(sys::nanosleep); // 35
        t[nr::GETITIMER] = Some(sys::getitimer); // 36
        t[nr::ALARM] = Some(sys::alarm); // 37
        t[nr::SETITIMER] = Some(sys::setitimer); // 38
        t[nr::GETPID] = Some(sys::getpid); // 39
        t[nr::SENDFILE] = Some(sys::sendfile); // 40
        t[nr::SOCKET] = Some(sys::socket); // 41
        t[nr::CONNECT] = Some(sys::connect); // 42
        t[nr::ACCEPT] = Some(sys::accept); // 43
        t[nr::SENDTO] = Some(sys::sendto); // 44
        t[nr::RECVFROM] = Some(sys::recvfrom); // 45
        t[nr::SENDMSG] = Some(sys::sendmsg); // 46
        t[nr::RECVMSG] = Some(sys::recvmsg); // 47
        t[nr::SHUTDOWN] = Some(sys::shutdown); // 48
        t[nr::BIND] = Some(sys::bind); // 49
        t[nr::LISTEN] = Some(sys::listen); // 50
        t[nr::GETSOCKNAME] = Some(sys::getsockname); // 51
        t[nr::GETPEERNAME] = Some(sys::getpeername); // 52
        t[nr::SOCKETPAIR] = Some(sys::socketpair); // 53
        t[nr::SETSOCKOPT] = Some(sys::setsockopt); // 54
        t[nr::GETSOCKOPT] = Some(sys::getsockopt); // 55
        t[nr::CLONE] = Some(sys::clone); // 56
        t[nr::FORK] = Some(sys::fork); // 57
        t[nr::VFORK] = Some(sys::vfork); // 58
        t[nr::EXECVE] = Some(sys::execve); // 59
        t[nr::EXIT] = Some(sys::exit); // 60
        t[nr::WAIT4] = Some(sys::wait4); // 61
        t[nr::KILL] = Some(sys::kill); // 62
        t[nr::UNAME] = Some(sys::uname); // 63
        t[nr::SEMGET] = Some(sys::semget); // 64
        t[nr::SEMOP] = Some(sys::semop); // 65
        t[nr::SEMCTL] = Some(sys::semctl); // 66
        t[nr::SHMDT] = Some(sys::shmdt); // 67
        t[nr::MSGGET] = Some(sys::msgget); // 68
        t[nr::MSGSND] = Some(sys::msgsnd); // 69
        t[nr::MSGRCV] = Some(sys::msgrcv); // 70
        t[nr::MSGCTL] = Some(sys::msgctl); // 71
        t[nr::FCNTL] = Some(sys::fcntl); // 72
        t[nr::FLOCK] = Some(sys::flock); // 73
        t[nr::FSYNC] = Some(sys::fsync); // 74
        t[nr::FDATASYNC] = Some(sys::fdatasync); // 75
        t[nr::TRUNCATE] = Some(sys::truncate); // 76
        t[nr::FTRUNCATE] = Some(sys::ftruncate); // 77
        t[nr::GETDENTS] = Some(sys::getdents); // 78
        t[nr::GETCWD] = Some(sys::getcwd); // 79
        t[nr::CHDIR] = Some(sys::chdir); // 80
        t[nr::FCHDIR] = Some(sys::fchdir); // 81
        t[nr::RENAME] = Some(sys::rename); // 82
        t[nr::MKDIR] = Some(sys::mkdir); // 83
        t[nr::RMDIR] = Some(sys::rmdir); // 84
        t[nr::CREAT] = Some(sys::creat); // 85
        t[nr::LINK] = Some(sys::link); // 86
        t[nr::UNLINK] = Some(sys::unlink); // 87
        t[nr::SYMLINK] = Some(sys::symlink); // 88
        t[nr::READLINK] = Some(sys::readlink); // 89
        t[nr::CHMOD] = Some(sys::chmod); // 90
        t[nr::FCHMOD] = Some(sys::fchmod); // 91
        t[nr::CHOWN] = Some(sys::chown); // 92
        t[nr::FCHOWN] = Some(sys::fchown); // 93
        t[nr::LCHOWN] = Some(sys::lchown); // 94
        t[nr::UMASK] = Some(sys::umask); // 95
        t[nr::GETTIMEOFDAY] = Some(sys::gettimeofday); // 96
        t[nr::GETRLIMIT] = Some(sys::getrlimit); // 97
        t[nr::GETRUSAGE] = Some(sys::getrusage); // 98
        t[nr::SYSINFO] = Some(sys::sysinfo); // 99
        t[nr::TIMES] = Some(sys::times); // 100
        t[nr::PTRACE] = Some(sys::ptrace); // 101
        t[nr::GETUID] = Some(sys::getuid); // 102
        t[nr::SYSLOG] = Some(sys::syslog); // 103
        t[nr::GETGID] = Some(sys::getgid); // 104
        t[nr::SETUID] = Some(sys::setuid); // 105
        t[nr::SETGID] = Some(sys::setgid); // 106
        t[nr::GETEUID] = Some(sys::geteuid); // 107
        t[nr::GETEGID] = Some(sys::getegid); // 108
        t[nr::SETPGID] = Some(sys::setpgid); // 109
        t[nr::GETPPID] = Some(sys::getppid); // 110
        t[nr::GETPGRP] = Some(sys::getpgrp); // 111
        t[nr::SETSID] = Some(sys::setsid); // 112
        t[nr::SETREUID] = Some(sys::setreuid); // 113
        t[nr::SETREGID] = Some(sys::setregid); // 114
        t[nr::GETGROUPS] = Some(sys::getgroups); // 115
        t[nr::SETGROUPS] = Some(sys::setgroups); // 116
        t[nr::SETRESUID] = Some(sys::setresuid); // 117
        t[nr::GETRESUID] = Some(sys::getresuid); // 118
        t[nr::SETRESGID] = Some(sys::setresgid); // 119
        t[nr::GETRESGID] = Some(sys::getresgid); // 120
        t[nr::GETPGID] = Some(sys::getpgid); // 121
        t[nr::SETFSUID] = Some(sys::setfsuid); // 122
        t[nr::SETFSGID] = Some(sys::setfsgid); // 123
        t[nr::GETSID] = Some(sys::getsid); // 124
        t[nr::CAPGET] = Some(sys::capget); // 125
        t[nr::CAPSET] = Some(sys::capset); // 126
        t[nr::RT_SIGPENDING] = Some(sys::rt_sigpending); // 127
        t[nr::RT_SIGTIMEDWAIT] = Some(sys::rt_sigtimedwait); // 128
        t[nr::RT_SIGQUEUEINFO] = Some(sys::rt_sigqueueinfo); // 129
        t[nr::RT_SIGSUSPEND] = Some(sys::rt_sigsuspend); // 130
        t[nr::SIGALTSTACK] = Some(sys::sigaltstack); // 131
        t[nr::UTIME] = Some(sys::utime); // 132
        t[nr::MKNOD] = Some(sys::mknod); // 133
        t[nr::USELIB] = Some(sys::uselib); // 134
        t[nr::PERSONALITY] = Some(sys::personality); // 135
        t[nr::USTAT] = Some(sys::ustat); // 136
        t[nr::STATFS] = Some(sys::statfs); // 137
        t[nr::FSTATFS] = Some(sys::fstatfs); // 138
        t[nr::SYSFS] = Some(sys::sysfs); // 139
        t[nr::GETPRIORITY] = Some(sys::getpriority); // 140
        t[nr::SETPRIORITY] = Some(sys::setpriority); // 141
        t[nr::SCHED_SETPARAM] = Some(sys::sched_setparam); // 142
        t[nr::SCHED_GETPARAM] = Some(sys::sched_getparam); // 143
        t[nr::SCHED_SETSCHEDULER] = Some(sys::sched_setscheduler); // 144
        t[nr::SCHED_GETSCHEDULER] = Some(sys::sched_getscheduler); // 145
        t[nr::SCHED_GET_PRIORITY_MAX] = Some(sys::sched_get_priority_max); // 146
        t[nr::SCHED_GET_PRIORITY_MIN] = Some(sys::sched_get_priority_min); // 147
        t[nr::SCHED_RR_GET_INTERVAL] = Some(sys::sched_rr_get_interval); // 148
        t[nr::MLOCK] = Some(sys::mlock); // 149
        t[nr::MUNLOCK] = Some(sys::munlock); // 150
        t[nr::MLOCKALL] = Some(sys::mlockall); // 151
        t[nr::MUNLOCKALL] = Some(sys::munlockall); // 152
        t[nr::VHANGUP] = Some(sys::vhangup); // 153
        t[nr::MODIFY_LDT] = Some(sys::modify_ldt); // 154
        t[nr::PIVOT_ROOT] = Some(sys::pivot_root); // 155
        t[nr::SYSCTL] = Some(sys::sysctl); // 156
        t[nr::PRCTL] = Some(sys::prctl); // 157
        t[nr::ARCH_PRCTL] = Some(sys::arch_prctl); // 158
        t[nr::ADJTIMEX] = Some(sys::adjtimex); // 159
        t[nr::SETRLIMIT] = Some(sys::setrlimit); // 160
        t[nr::CHROOT] = Some(sys::chroot); // 161
        t[nr::SYNC] = Some(sys::sync); // 162
        t[nr::ACCT] = Some(sys::acct); // 163
        t[nr::SETTIMEOFDAY] = Some(sys::settimeofday); // 164
        t[nr::MOUNT] = Some(sys::mount); // 165
        t[nr::UMOUNT2] = Some(sys::umount); // 166
        t[nr::SWAPON] = Some(sys::swapon); // 167
        t[nr::SWAPOFF] = Some(sys::swapoff); // 168
        t[nr::REBOOT] = Some(sys::reboot); // 169
        t[nr::SETHOSTNAME] = Some(sys::sethostname); // 170
        t[nr::SETDOMAINNAME] = Some(sys::setdomainname); // 171
        t[nr::IOPL] = Some(sys::iopl); // 172
        t[nr::IOPERM] = Some(sys::ioperm); // 173
        t[nr::CREATE_MODULE] = Some(sys::create_module); // 174
        t[nr::INIT_MODULE] = Some(sys::init_module); // 175
        t[nr::DELETE_MODULE] = Some(sys::delete_module); // 176
        t[nr::GET_KERNEL_SYMS] = Some(sys::get_kernel_syms); // 177
        t[nr::QUERY_MODULE] = Some(sys::query_module); // 178
        t[nr::QUOTACTL] = Some(sys::quotactl); // 179
        t[nr::NFSSERVCTL] = Some(sys::nfsservctl); // 180
        t[nr::GETPMSG] = Some(sys::getpmsg); // 181
        t[nr::PUTPMSG] = Some(sys::putpmsg); // 182
        t[nr::AFS_SYSCALL] = Some(sys::afs_syscall); // 183
        t[nr::TUXCALL] = Some(sys::tuxcall); // 184
        t[nr::SECURITY] = Some(sys::security); // 185
        t[nr::GETTID] = Some(sys::gettid); // 186
        t[nr::READAHEAD] = Some(sys::readahead); // 187
        t[nr::SETXATTR] = Some(sys::setxattr); // 188
        t[nr::LSETXATTR] = Some(sys::lsetxattr); // 189
        t[nr::FSETXATTR] = Some(sys::fsetxattr); // 190
        t[nr::GETXATTR] = Some(sys::getxattr); // 191
        t[nr::LGETXATTR] = Some(sys::lgetxattr); // 192
        t[nr::FGETXATTR] = Some(sys::fgetxattr); // 193
        t[nr::LISTXATTR] = Some(sys::listxattr); // 194
        t[nr::LLISTXATTR] = Some(sys::llistxattr); // 195
        t[nr::FLISTXATTR] = Some(sys::flistxattr); // 196
        t[nr::REMOVEXATTR] = Some(sys::removexattr); // 197
        t[nr::LREMOVEXATTR] = Some(sys::lremovexattr); // 198
        t[nr::FREMOVEXATTR] = Some(sys::fremovexattr); // 199
        t[nr::TKILL] = Some(sys::tkill); // 200
        t[nr::TIME] = Some(sys::time); // 201
        t[nr::FUTEX] = Some(sys::futex); // 202
        t[nr::SCHED_SETAFFINITY] = Some(sys::sched_setaffinity); // 203
        t[nr::SCHED_GETAFFINITY] = Some(sys::sched_getaffinity); // 204
        t[nr::SET_THREAD_AREA] = Some(sys::set_thread_area); // 205
        t[nr::IO_SETUP] = Some(sys::io_setup); // 206
        t[nr::IO_DESTROY] = Some(sys::io_destroy); // 207
        t[nr::IO_GETEVENTS] = Some(sys::io_getevents); // 208
        t[nr::IO_SUBMIT] = Some(sys::io_submit); // 209
        t[nr::IO_CANCEL] = Some(sys::io_cancel); // 210
        t[nr::GET_THREAD_AREA] = Some(sys::get_thread_area); // 211
        t[nr::LOOKUP_DCOOKIE] = Some(sys::lookup_dcookie); // 212
        t[nr::EPOLL_CREATE] = Some(sys::epoll_create); // 213
        t[nr::EPOLL_CTL_OLD] = Some(sys::epoll_ctl_old); // 214
        t[nr::EPOLL_WAIT_OLD] = Some(sys::epoll_wait_old); // 215
        t[nr::REMAP_FILE_PAGES] = Some(sys::remap_file_pages); // 216
        t[nr::GETDENTS64] = Some(sys::getdents64); // 217
        t[nr::SET_TID_ADDRESS] = Some(sys::set_tid_address); // 218
        t[nr::RESTART_SYSCALL] = Some(sys::restart_syscall); // 219
        t[nr::SEMTIMEDOP] = Some(sys::semtimedop); // 220
        t[nr::FADVISE64] = Some(sys::fadvise64); // 221
        t[nr::TIMER_CREATE] = Some(sys::timer_create); // 222
        t[nr::TIMER_SETTIME] = Some(sys::timer_settime); // 223
        t[nr::TIMER_GETTIME] = Some(sys::timer_gettime); // 224
        t[nr::TIMER_GETOVERRUN] = Some(sys::timer_getoverrun); // 225
        t[nr::TIMER_DELETE] = Some(sys::timer_delete); // 226
        t[nr::CLOCK_SETTIME] = Some(sys::clock_settime); // 227
        t[nr::CLOCK_GETTIME] = Some(sys::clock_gettime); // 228
        t[nr::CLOCK_GETRES] = Some(sys::clock_getres); // 229
        t[nr::CLOCK_NANOSLEEP] = Some(sys::clock_nanosleep); // 230
        t[nr::EXIT_GROUP] = Some(sys::exit_group); // 231
        t[nr::EPOLL_WAIT] = Some(sys::epoll_wait); // 232
        t[nr::EPOLL_CTL] = Some(sys::epoll_ctl); // 233
        t[nr::TGKILL] = Some(sys::tgkill); // 234
        t[nr::UTIMES] = Some(sys::utimes); // 235
        t[nr::VSERVER] = Some(sys::vserver); // 236
        t[nr::MBIND] = Some(sys::mbind); // 237
        t[nr::SET_MEMPOLICY] = Some(sys::set_mempolicy); // 238
        t[nr::GET_MEMPOLICY] = Some(sys::get_mempolicy); // 239
        t[nr::MQ_OPEN] = Some(sys::mq_open); // 240
        t[nr::MQ_UNLINK] = Some(sys::mq_unlink); // 241
        t[nr::MQ_TIMEDSEND] = Some(sys::mq_timedsend); // 242
        t[nr::MQ_TIMEDRECEIVE] = Some(sys::mq_timedreceive); // 243
        t[nr::MQ_NOTIFY] = Some(sys::mq_notify); // 244
        t[nr::MQ_GETSETATTR] = Some(sys::mq_getsetattr); // 245
        t[nr::KEXEC_LOAD] = Some(sys::kexec_load); // 246
        t[nr::WAITID] = Some(sys::waitid); // 247
        t[nr::ADD_KEY] = Some(sys::add_key); // 248
        t[nr::REQUEST_KEY] = Some(sys::request_key); // 249
        t[nr::KEYCTL] = Some(sys::keyctl); // 250
        t[nr::IOPRIO_SET] = Some(sys::ioprio_set); // 251
        t[nr::IOPRIO_GET] = Some(sys::ioprio_get); // 252
        t[nr::INOTIFY_INIT] = Some(sys::inotify_init); // 253
        t[nr::INOTIFY_ADD_WATCH] = Some(sys::inotify_add_watch); // 254
        t[nr::INOTIFY_RM_WATCH] = Some(sys::inotify_rm_watch); // 255
        t[nr::MIGRATE_PAGES] = Some(sys::migrate_pages); // 256
        t[nr::OPENAT] = Some(sys::openat); // 257
        t[nr::MKDIRAT] = Some(sys::mkdirat); // 258
        t[nr::MKNODAT] = Some(sys::mknodat); // 259
        t[nr::FCHOWNAT] = Some(sys::fchownat); // 260
        t[nr::FUTIMESAT] = Some(sys::futimesat); // 261
        t[nr::NEWFSTATAT] = Some(sys::newfstatat); // 262
        t[nr::UNLINKAT] = Some(sys::unlinkat); // 263
        t[nr::RENAMEAT] = Some(sys::renameat); // 264
        t[nr::LINKAT] = Some(sys::linkat); // 265
        t[nr::SYMLINKAT] = Some(sys::symlinkat); // 266
        t[nr::READLINKAT] = Some(sys::readlinkat); // 267
        t[nr::FCHMODAT] = Some(sys::fchmodat); // 268
        t[nr::FACCESSAT] = Some(sys::faccessat); // 269
        t[nr::PSELECT6] = Some(sys::pselect6); // 270
        t[nr::PPOLL] = Some(sys::ppoll); // 271
        t[nr::UNSHARE] = Some(sys::unshare); // 272
        t[nr::SET_ROBUST_LIST] = Some(sys::set_robust_list); // 273
        t[nr::GET_ROBUST_LIST] = Some(sys::get_robust_list); // 274
        t[nr::SPLICE] = Some(sys::splice); // 275
        t[nr::TEE] = Some(sys::tee); // 276
        t[nr::SYNC_FILE_RANGE] = Some(sys::sync_file_range); // 277
        t[nr::VMSPLICE] = Some(sys::vmsplice); // 278
        t[nr::MOVE_PAGES] = Some(sys::move_pages); // 279
        t[nr::UTIMENSAT] = Some(sys::utimensat); // 280
        t[nr::EPOLL_PWAIT] = Some(sys::epoll_pwait); // 281
        t[nr::SIGNALFD] = Some(sys::signalfd); // 282
        t[nr::TIMERFD_CREATE] = Some(sys::timerfd_create); // 283
        t[nr::EVENTFD] = Some(sys::eventfd); // 284
        t[nr::FALLOCATE] = Some(sys::fallocate); // 285
        t[nr::TIMERFD_SETTIME] = Some(sys::timerfd_settime); // 286
        t[nr::TIMERFD_GETTIME] = Some(sys::timerfd_gettime); // 287
        t[nr::ACCEPT4] = Some(sys::accept4); // 288
        t[nr::SIGNALFD4] = Some(sys::signalfd4); // 289
        t[nr::EVENTFD2] = Some(sys::eventfd2); // 290
        t[nr::EPOLL_CREATE1] = Some(sys::epoll_create1); // 291
        t[nr::DUP3] = Some(sys::dup3); // 292
        t[nr::PIPE2] = Some(sys::pipe2); // 293
        t[nr::INOTIFY_INIT1] = Some(sys::inotify_init1); // 294
        t[nr::PREADV] = Some(sys::preadv); // 295
        t[nr::PWRITEV] = Some(sys::pwritev); // 296
        t[nr::RT_TGSIGQUEUEINFO] = Some(sys::rt_tgsigqueueinfo); // 297
        t[nr::PERF_EVENT_OPEN] = Some(sys::perf_event_open); // 298
        t[nr::RECVMMSG] = Some(sys::recvmmsg); // 299
        t[nr::FANOTIFY_INIT] = Some(sys::fanotify_init); // 300
        t[nr::FANOTIFY_MARK] = Some(sys::fanotify_mark); // 301
        t[nr::PRLIMIT64] = Some(sys::prlimit64); // 302
        t[nr::NAME_TO_HANDLE_AT] = Some(sys::name_to_handle_at); // 303
        t[nr::OPEN_BY_HANDLE_AT] = Some(sys::open_by_handle_at); // 304
        t[nr::CLOCK_ADJTIME] = Some(sys::clock_adjtime); // 305
        t[nr::SYNCFS] = Some(sys::syncfs); // 306
        t[nr::SENDMMSG] = Some(sys::sendmmsg); // 307
        t[nr::SETNS] = Some(sys::setns); // 308
        t[nr::GETCPU] = Some(sys::getcpu); // 309
        t[nr::PROCESS_VM_READV] = Some(sys::process_vm_readv); // 310
        t[nr::PROCESS_VM_WRITEV] = Some(sys::process_vm_writev); // 311
        t[nr::KCMP] = Some(sys::kcmp); // 312
        t[nr::FINIT_MODULE] = Some(sys::finit_module); // 313
        t[nr::SCHED_SETATTR] = Some(sys::sched_setattr); // 314
        t[nr::SCHED_GETATTR] = Some(sys::sched_getattr); // 315
        t[nr::RENAMEAT2] = Some(sys::renameat2); // 316
        t[nr::SECCOMP] = Some(sys::seccomp); // 317
        t[nr::GETRANDOM] = Some(sys::getrandom); // 318
        t[nr::MEMFD_CREATE] = Some(sys::memfd_create); // 319
        t[nr::KEXEC_FILE_LOAD] = Some(sys::kexec_file_load); // 320
        t[nr::BPF] = Some(sys::bpf); // 321
        t[nr::EXECVEAT] = Some(sys::execveat); // 322
        t[nr::USERFAULTFD] = Some(sys::userfaultfd); // 323
        t[nr::MEMBARRIER] = Some(sys::membarrier); // 324
        t[nr::MLOCK2] = Some(sys::mlock2); // 325
        t[nr::COPY_FILE_RANGE] = Some(sys::copy_file_range); // 326
        t[nr::PREADV2] = Some(sys::preadv2); // 327
        t[nr::PWRITEV2] = Some(sys::pwritev2); // 328
        t[nr::PKEY_MPROTECT] = Some(sys::pkey_mprotect); // 329
        t[nr::PKEY_ALLOC] = Some(sys::pkey_alloc); // 330
        t[nr::PKEY_FREE] = Some(sys::pkey_free); // 331
        t[nr::STATX] = Some(sys::statx); // 332
        t[nr::IO_PGETEVENTS] = Some(sys::io_pgetevents); // 333
        t[nr::RSEQ] = Some(sys::rseq); // 334
        t[nr::PIDFD_SEND_SIGNAL] = Some(sys::pidfd_send_signal); // 424
        t[nr::OPEN_TREE] = Some(sys::open_tree); // 428
        t[nr::MOVE_MOUNT] = Some(sys::move_mount); // 429
        t[nr::FSOPEN] = Some(sys::fsopen); // 430
        t[nr::FSCONFIG] = Some(sys::fsconfig); // 431
        t[nr::FSMOUNT] = Some(sys::fsmount); // 432
        t[nr::FSPICK] = Some(sys::fspick); // 433
        t[nr::CLOSE_RANGE] = Some(sys::close_range); // 436
        t[nr::OPENAT2] = Some(sys::openat2); // 437
        t[nr::PIDFD_GETFD] = Some(sys::pidfd_getfd); // 438
        t[nr::PROCESS_MADVISE] = Some(sys::process_madvise); // 440
        t[nr::MOUNT_SETATTR] = Some(sys::mount_setattr); // 442
        t[nr::QUOTACTL_FD] = Some(sys::quotactl_fd); // 443
        t[nr::LANDLOCK_CREATE_RULESET] = Some(sys::landlock_create_ruleset); // 444
        t[nr::LANDLOCK_ADD_RULE] = Some(sys::landlock_add_rule); // 445
        t[nr::LANDLOCK_RESTRICT_SELF] = Some(sys::landlock_restrict_self); // 446
        t[nr::MEMFD_SECRET] = Some(sys::memfd_secret); // 447
        t[nr::PROCESS_MRELEASE] = Some(sys::process_mrelease); // 448
        t[nr::IO_URING_SETUP] = Some(sys::io_uring_setup); // 425
        t[nr::IO_URING_ENTER] = Some(sys::io_uring_enter); // 426
        t[nr::IO_URING_REGISTER] = Some(sys::io_uring_register); // 427
        t[nr::PIDFD_OPEN] = Some(sys::pidfd_open); // 434
        t[nr::CLONE3] = Some(sys::clone3); // 435
        t[nr::FACCESSAT2] = Some(sys::faccessat2); // 439
        t[nr::EPOLL_PWAIT2] = Some(sys::epoll_pwait2); // 441
        t[nr::IDLE] = Some(sys::idle); // 500，自检专用
    }
}

/// 哪些调用号挂了实现。与 [`SYS_CALL_TABLE`] 的赋值逐项对应，由同一份
/// 生成逻辑产出，见 [`implemented_count`] 说明为何不直接比函数指针。
static WIRED: [bool; nr::NR_SYSCALLS] = {
    let mut w = [false; nr::NR_SYSCALLS];
    w[nr::READ] = true;
    w[nr::WRITE] = true;
    w[nr::OPEN] = true;
    w[nr::CLOSE] = true;
    w[nr::STAT] = true;
    w[nr::FSTAT] = true;
    w[nr::LSTAT] = true;
    w[nr::POLL] = true;
    w[nr::LSEEK] = true;
    w[nr::MMAP] = true;
    w[nr::MPROTECT] = true;
    w[nr::MUNMAP] = true;
    w[nr::BRK] = true;
    w[nr::RT_SIGACTION] = true;
    w[nr::RT_SIGPROCMASK] = true;
    w[nr::RT_SIGRETURN] = true;
    w[nr::IOCTL] = true;
    w[nr::PREAD64] = true;
    w[nr::PWRITE64] = true;
    w[nr::READV] = true;
    w[nr::WRITEV] = true;
    w[nr::ACCESS] = true;
    w[nr::PIPE] = true;
    w[nr::SELECT] = true;
    w[nr::SCHED_YIELD] = true;
    w[nr::MREMAP] = true;
    w[nr::MSYNC] = true;
    w[nr::MINCORE] = true;
    w[nr::MADVISE] = true;
    w[nr::SHMGET] = true;
    w[nr::SHMAT] = true;
    w[nr::SHMCTL] = true;
    w[nr::DUP] = true;
    w[nr::DUP2] = true;
    w[nr::PAUSE] = true;
    w[nr::NANOSLEEP] = true;
    w[nr::GETITIMER] = true;
    w[nr::ALARM] = true;
    w[nr::SETITIMER] = true;
    w[nr::GETPID] = true;
    w[nr::SENDFILE] = true;
    w[nr::SOCKET] = true;
    w[nr::CONNECT] = true;
    w[nr::ACCEPT] = true;
    w[nr::SENDTO] = true;
    w[nr::RECVFROM] = true;
    w[nr::SENDMSG] = true;
    w[nr::RECVMSG] = true;
    w[nr::SHUTDOWN] = true;
    w[nr::BIND] = true;
    w[nr::LISTEN] = true;
    w[nr::GETSOCKNAME] = true;
    w[nr::GETPEERNAME] = true;
    w[nr::SOCKETPAIR] = true;
    w[nr::SETSOCKOPT] = true;
    w[nr::GETSOCKOPT] = true;
    w[nr::CLONE] = true;
    w[nr::FORK] = true;
    w[nr::VFORK] = true;
    w[nr::EXECVE] = true;
    w[nr::EXIT] = true;
    w[nr::WAIT4] = true;
    w[nr::KILL] = true;
    w[nr::UNAME] = true;
    w[nr::SEMGET] = true;
    w[nr::SEMOP] = true;
    w[nr::SEMCTL] = true;
    w[nr::SHMDT] = true;
    w[nr::MSGGET] = true;
    w[nr::MSGSND] = true;
    w[nr::MSGRCV] = true;
    w[nr::MSGCTL] = true;
    w[nr::FCNTL] = true;
    w[nr::FLOCK] = true;
    w[nr::FSYNC] = true;
    w[nr::FDATASYNC] = true;
    w[nr::TRUNCATE] = true;
    w[nr::FTRUNCATE] = true;
    w[nr::GETDENTS] = true;
    w[nr::GETCWD] = true;
    w[nr::CHDIR] = true;
    w[nr::FCHDIR] = true;
    w[nr::RENAME] = true;
    w[nr::MKDIR] = true;
    w[nr::RMDIR] = true;
    w[nr::CREAT] = true;
    w[nr::LINK] = true;
    w[nr::UNLINK] = true;
    w[nr::SYMLINK] = true;
    w[nr::READLINK] = true;
    w[nr::CHMOD] = true;
    w[nr::FCHMOD] = true;
    w[nr::CHOWN] = true;
    w[nr::FCHOWN] = true;
    w[nr::LCHOWN] = true;
    w[nr::UMASK] = true;
    w[nr::GETTIMEOFDAY] = true;
    w[nr::GETRLIMIT] = true;
    w[nr::GETRUSAGE] = true;
    w[nr::SYSINFO] = true;
    w[nr::TIMES] = true;
    w[nr::PTRACE] = true;
    w[nr::GETUID] = true;
    w[nr::SYSLOG] = true;
    w[nr::GETGID] = true;
    w[nr::SETUID] = true;
    w[nr::SETGID] = true;
    w[nr::GETEUID] = true;
    w[nr::GETEGID] = true;
    w[nr::SETPGID] = true;
    w[nr::GETPPID] = true;
    w[nr::GETPGRP] = true;
    w[nr::SETSID] = true;
    w[nr::SETREUID] = true;
    w[nr::SETREGID] = true;
    w[nr::GETGROUPS] = true;
    w[nr::SETGROUPS] = true;
    w[nr::SETRESUID] = true;
    w[nr::GETRESUID] = true;
    w[nr::SETRESGID] = true;
    w[nr::GETRESGID] = true;
    w[nr::GETPGID] = true;
    w[nr::SETFSUID] = true;
    w[nr::SETFSGID] = true;
    w[nr::GETSID] = true;
    w[nr::CAPGET] = true;
    w[nr::CAPSET] = true;
    w[nr::RT_SIGPENDING] = true;
    w[nr::RT_SIGTIMEDWAIT] = true;
    w[nr::RT_SIGQUEUEINFO] = true;
    w[nr::RT_SIGSUSPEND] = true;
    w[nr::SIGALTSTACK] = true;
    w[nr::UTIME] = true;
    w[nr::MKNOD] = true;
    w[nr::USELIB] = true;
    w[nr::PERSONALITY] = true;
    w[nr::USTAT] = true;
    w[nr::STATFS] = true;
    w[nr::FSTATFS] = true;
    w[nr::SYSFS] = true;
    w[nr::GETPRIORITY] = true;
    w[nr::SETPRIORITY] = true;
    w[nr::SCHED_SETPARAM] = true;
    w[nr::SCHED_GETPARAM] = true;
    w[nr::SCHED_SETSCHEDULER] = true;
    w[nr::SCHED_GETSCHEDULER] = true;
    w[nr::SCHED_GET_PRIORITY_MAX] = true;
    w[nr::SCHED_GET_PRIORITY_MIN] = true;
    w[nr::SCHED_RR_GET_INTERVAL] = true;
    w[nr::MLOCK] = true;
    w[nr::MUNLOCK] = true;
    w[nr::MLOCKALL] = true;
    w[nr::MUNLOCKALL] = true;
    w[nr::VHANGUP] = true;
    w[nr::MODIFY_LDT] = true;
    w[nr::PIVOT_ROOT] = true;
    w[nr::SYSCTL] = true;
    w[nr::PRCTL] = true;
    w[nr::ARCH_PRCTL] = true;
    w[nr::ADJTIMEX] = true;
    w[nr::SETRLIMIT] = true;
    w[nr::CHROOT] = true;
    w[nr::SYNC] = true;
    w[nr::ACCT] = true;
    w[nr::SETTIMEOFDAY] = true;
    w[nr::MOUNT] = true;
    w[nr::UMOUNT2] = true;
    w[nr::SWAPON] = true;
    w[nr::SWAPOFF] = true;
    w[nr::REBOOT] = true;
    w[nr::SETHOSTNAME] = true;
    w[nr::SETDOMAINNAME] = true;
    w[nr::IOPL] = true;
    w[nr::IOPERM] = true;
    w[nr::CREATE_MODULE] = true;
    w[nr::INIT_MODULE] = true;
    w[nr::DELETE_MODULE] = true;
    w[nr::GET_KERNEL_SYMS] = true;
    w[nr::QUERY_MODULE] = true;
    w[nr::QUOTACTL] = true;
    w[nr::NFSSERVCTL] = true;
    w[nr::GETPMSG] = true;
    w[nr::PUTPMSG] = true;
    w[nr::AFS_SYSCALL] = true;
    w[nr::TUXCALL] = true;
    w[nr::SECURITY] = true;
    w[nr::GETTID] = true;
    w[nr::READAHEAD] = true;
    w[nr::SETXATTR] = true;
    w[nr::LSETXATTR] = true;
    w[nr::FSETXATTR] = true;
    w[nr::GETXATTR] = true;
    w[nr::LGETXATTR] = true;
    w[nr::FGETXATTR] = true;
    w[nr::LISTXATTR] = true;
    w[nr::LLISTXATTR] = true;
    w[nr::FLISTXATTR] = true;
    w[nr::REMOVEXATTR] = true;
    w[nr::LREMOVEXATTR] = true;
    w[nr::FREMOVEXATTR] = true;
    w[nr::TKILL] = true;
    w[nr::TIME] = true;
    w[nr::FUTEX] = true;
    w[nr::SCHED_SETAFFINITY] = true;
    w[nr::SCHED_GETAFFINITY] = true;
    w[nr::SET_THREAD_AREA] = true;
    w[nr::IO_SETUP] = true;
    w[nr::IO_DESTROY] = true;
    w[nr::IO_GETEVENTS] = true;
    w[nr::IO_SUBMIT] = true;
    w[nr::IO_CANCEL] = true;
    w[nr::GET_THREAD_AREA] = true;
    w[nr::LOOKUP_DCOOKIE] = true;
    w[nr::EPOLL_CREATE] = true;
    w[nr::EPOLL_CTL_OLD] = true;
    w[nr::EPOLL_WAIT_OLD] = true;
    w[nr::REMAP_FILE_PAGES] = true;
    w[nr::GETDENTS64] = true;
    w[nr::SET_TID_ADDRESS] = true;
    w[nr::RESTART_SYSCALL] = true;
    w[nr::SEMTIMEDOP] = true;
    w[nr::FADVISE64] = true;
    w[nr::TIMER_CREATE] = true;
    w[nr::TIMER_SETTIME] = true;
    w[nr::TIMER_GETTIME] = true;
    w[nr::TIMER_GETOVERRUN] = true;
    w[nr::TIMER_DELETE] = true;
    w[nr::CLOCK_SETTIME] = true;
    w[nr::CLOCK_GETTIME] = true;
    w[nr::CLOCK_GETRES] = true;
    w[nr::CLOCK_NANOSLEEP] = true;
    w[nr::EXIT_GROUP] = true;
    w[nr::EPOLL_WAIT] = true;
    w[nr::EPOLL_CTL] = true;
    w[nr::TGKILL] = true;
    w[nr::UTIMES] = true;
    w[nr::VSERVER] = true;
    w[nr::MBIND] = true;
    w[nr::SET_MEMPOLICY] = true;
    w[nr::GET_MEMPOLICY] = true;
    w[nr::MQ_OPEN] = true;
    w[nr::MQ_UNLINK] = true;
    w[nr::MQ_TIMEDSEND] = true;
    w[nr::MQ_TIMEDRECEIVE] = true;
    w[nr::MQ_NOTIFY] = true;
    w[nr::MQ_GETSETATTR] = true;
    w[nr::KEXEC_LOAD] = true;
    w[nr::WAITID] = true;
    w[nr::ADD_KEY] = true;
    w[nr::REQUEST_KEY] = true;
    w[nr::KEYCTL] = true;
    w[nr::IOPRIO_SET] = true;
    w[nr::IOPRIO_GET] = true;
    w[nr::INOTIFY_INIT] = true;
    w[nr::INOTIFY_ADD_WATCH] = true;
    w[nr::INOTIFY_RM_WATCH] = true;
    w[nr::MIGRATE_PAGES] = true;
    w[nr::OPENAT] = true;
    w[nr::MKDIRAT] = true;
    w[nr::MKNODAT] = true;
    w[nr::FCHOWNAT] = true;
    w[nr::FUTIMESAT] = true;
    w[nr::NEWFSTATAT] = true;
    w[nr::UNLINKAT] = true;
    w[nr::RENAMEAT] = true;
    w[nr::LINKAT] = true;
    w[nr::SYMLINKAT] = true;
    w[nr::READLINKAT] = true;
    w[nr::FCHMODAT] = true;
    w[nr::FACCESSAT] = true;
    w[nr::PSELECT6] = true;
    w[nr::PPOLL] = true;
    w[nr::UNSHARE] = true;
    w[nr::SET_ROBUST_LIST] = true;
    w[nr::GET_ROBUST_LIST] = true;
    w[nr::SPLICE] = true;
    w[nr::TEE] = true;
    w[nr::SYNC_FILE_RANGE] = true;
    w[nr::VMSPLICE] = true;
    w[nr::MOVE_PAGES] = true;
    w[nr::UTIMENSAT] = true;
    w[nr::EPOLL_PWAIT] = true;
    w[nr::SIGNALFD] = true;
    w[nr::TIMERFD_CREATE] = true;
    w[nr::EVENTFD] = true;
    w[nr::FALLOCATE] = true;
    w[nr::TIMERFD_SETTIME] = true;
    w[nr::TIMERFD_GETTIME] = true;
    w[nr::ACCEPT4] = true;
    w[nr::SIGNALFD4] = true;
    w[nr::EVENTFD2] = true;
    w[nr::EPOLL_CREATE1] = true;
    w[nr::DUP3] = true;
    w[nr::PIPE2] = true;
    w[nr::INOTIFY_INIT1] = true;
    w[nr::PREADV] = true;
    w[nr::PWRITEV] = true;
    w[nr::RT_TGSIGQUEUEINFO] = true;
    w[nr::PERF_EVENT_OPEN] = true;
    w[nr::RECVMMSG] = true;
    w[nr::FANOTIFY_INIT] = true;
    w[nr::FANOTIFY_MARK] = true;
    w[nr::PRLIMIT64] = true;
    w[nr::NAME_TO_HANDLE_AT] = true;
    w[nr::OPEN_BY_HANDLE_AT] = true;
    w[nr::CLOCK_ADJTIME] = true;
    w[nr::SYNCFS] = true;
    w[nr::SENDMMSG] = true;
    w[nr::SETNS] = true;
    w[nr::GETCPU] = true;
    w[nr::PROCESS_VM_READV] = true;
    w[nr::PROCESS_VM_WRITEV] = true;
    w[nr::KCMP] = true;
    w[nr::FINIT_MODULE] = true;
    w[nr::SCHED_SETATTR] = true;
    w[nr::SCHED_GETATTR] = true;
    w[nr::RENAMEAT2] = true;
    w[nr::SECCOMP] = true;
    w[nr::GETRANDOM] = true;
    w[nr::MEMFD_CREATE] = true;
    w[nr::KEXEC_FILE_LOAD] = true;
    w[nr::BPF] = true;
    w[nr::EXECVEAT] = true;
    w[nr::USERFAULTFD] = true;
    w[nr::MEMBARRIER] = true;
    w[nr::MLOCK2] = true;
    w[nr::COPY_FILE_RANGE] = true;
    w[nr::PREADV2] = true;
    w[nr::PWRITEV2] = true;
    w[nr::PKEY_MPROTECT] = true;
    w[nr::PKEY_ALLOC] = true;
    w[nr::PKEY_FREE] = true;
    w[nr::STATX] = true;
    w[nr::IO_PGETEVENTS] = true;
    w[nr::RSEQ] = true;
    w[nr::PIDFD_SEND_SIGNAL] = true;
    w[nr::OPEN_TREE] = true;
    w[nr::MOVE_MOUNT] = true;
    w[nr::FSOPEN] = true;
    w[nr::FSCONFIG] = true;
    w[nr::FSMOUNT] = true;
    w[nr::FSPICK] = true;
    w[nr::CLOSE_RANGE] = true;
    w[nr::OPENAT2] = true;
    w[nr::PIDFD_GETFD] = true;
    w[nr::PROCESS_MADVISE] = true;
    w[nr::MOUNT_SETATTR] = true;
    w[nr::QUOTACTL_FD] = true;
    w[nr::LANDLOCK_CREATE_RULESET] = true;
    w[nr::LANDLOCK_ADD_RULE] = true;
    w[nr::LANDLOCK_RESTRICT_SELF] = true;
    w[nr::MEMFD_SECRET] = true;
    w[nr::PROCESS_MRELEASE] = true;
    w[nr::IO_URING_SETUP] = true;
    w[nr::IO_URING_ENTER] = true;
    w[nr::IO_URING_REGISTER] = true;
    w[nr::PIDFD_OPEN] = true;
    w[nr::CLONE3] = true;
    w[nr::FACCESSAT2] = true;
    w[nr::EPOLL_PWAIT2] = true;
    w[nr::IDLE] = true;
    w
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

    // seccomp strict（SECCOMP_SET_MODE_STRICT）：只剩 read/write/exit/
    // rt_sigreturn/exit_group 可用，其余调用直接 SIGKILL——不执行该调用。
    if cur.seccomp_strict
        && !matches!(call_nr,
            nr::READ | nr::WRITE | nr::EXIT | nr::RT_SIGRETURN | nr::EXIT_GROUP)
    {
        crate::signal::send_sig_current(9); // SIGKILL
        regs.rax = 0;
        set_carry(regs, false);
        return;
    }

    let args = SysArgs::from_regs(regs);

    // SAFETY: init_table 在启动早期已填表；None 走 ni_syscall。
    let f = unsafe { (*core::ptr::addr_of!(SYS_CALL_TABLE))[call_nr] }
        .unwrap_or(sys::ni_syscall);
    let ret = f(&args, regs);

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

/// 4 参数版本（epoll_ctl/epoll_wait 等需要 r10 传第 4 参的用）。
///
/// # Safety
/// 同 [`syscall3`]。
pub unsafe fn syscall4(number: usize, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    // SAFETY: 同 syscall3；a3 走 r10（x86_64 系统调用第 4 参约定）。
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") number as u64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
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

/// 六参数版本（rdi/rsi/rdx/r10/r8/r9），给 splice/sendfile 等自检用。
///
/// # Safety
/// 同 [`syscall3`]。
pub unsafe fn syscall6(
    number: usize,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> i64 {
    let ret: i64;
    // SAFETY: 契约保证 IDT 就绪。参数按 x86_64 系统调用 ABI 放到对应寄存器。
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") number as u64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            in("r9") a5,
        );
    }
    ret
}

/// 表里挂了实现（不是 [`sys::ni_syscall`]）的槽位数。
///
/// 不能靠比较函数指针来数：release 下 LLVM 会把函数体相同的 `sys_*` 合并成
/// 同一个地址（identical code folding），返回 `-EINVAL` 的占位实现会和
/// `ni_syscall` 撞成一个地址，数出来比实际少。所以另建一张 [`WIRED`] 位图，
/// 与分发表在同一个 const 块里赋值，数的是「这个号有没有被显式赋过实现」。
pub fn implemented_count() -> usize {
    WIRED.iter().filter(|w| **w).count()
}

/// 某个调用号是否挂了实现。
pub fn is_implemented(nr: usize) -> bool {
    nr < nr::NR_SYSCALLS && WIRED[nr]
}

/// 打印系统调用统计，供启动自检。
pub fn dump() {
    crate::pr!(Level::Info, "syscall: table={} slots, {} wired, {} calls served",
               nr::NR_SYSCALLS, implemented_count(), syscall_count());
}
