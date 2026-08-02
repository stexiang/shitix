//! 系统调用的具体实现。对应 linux-1.0.9 的 `kernel/sys.c` 与
//! `kernel/sched.c` 里那些 `sys_*` 函数。
//!
//! 这里只实现不依赖 `fs/`、`kernel/signal.c`、`mm/mmap.c` 的那些，
//! 其余在分发表里指向 [`ni_syscall`]。每个函数的文档注明原版位置。

use super::{SysArgs, nr};
use crate::klib::errno::{EFAULT, EINVAL, ENOSYS};
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
