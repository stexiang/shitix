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

/// 系统调用号。取值与原版 `include/linux/unistd.h` 的 `__NR_*` 完全一致
/// （即 i386 的调用号），这样将来跑原版编译出的用户程序也能对上。
pub mod nr {
    pub const SETUP: usize = 0;
    pub const EXIT: usize = 1;
    pub const FORK: usize = 2;
    pub const READ: usize = 3;
    pub const WRITE: usize = 4;
    pub const OPEN: usize = 5;
    pub const CLOSE: usize = 6;
    pub const WAITPID: usize = 7;
    pub const GETPID: usize = 20;
    pub const PAUSE: usize = 29;
    pub const KILL: usize = 37;
    pub const DUP: usize = 41;
    pub const TIMES: usize = 43;
    pub const GETPPID: usize = 64;
    pub const GETPGRP: usize = 65;
    pub const SETSID: usize = 66;
    pub const UNAME: usize = 122;
    pub const IDLE: usize = 112;
    pub const GETPGID: usize = 132;
    /// 表的容量。原版 `NR_syscalls = sizeof(sys_call_table)/sizeof(fn_ptr)` = 137。
    pub const NR_SYSCALLS: usize = 137;
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
