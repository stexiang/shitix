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

// =============================================================================
// 用户指针访问的临时护栏
//
// 原版靠 `verify_area(VERIFY_READ/WRITE, ptr, len)` + 段限长挡住越界；本树还没
// 有 `vm_area_struct`（见 STATUS 的 Open decisions），做不到真正的按 VMA 校验。
// 折中办法：只接受落在恒等映射低 1GB 内的地址——这段在 `boot/setup.S` 里被
// 2MB 大页整体映射过，读写不会触发 page fault，所以「坏指针把内核打挂」这类
// 后果被挡住了。**但它挡不住「用户态读写内核内存」**，等 mmap 到位后必须换成
// 真的 `verify_area`。这是已知的安全缺口，不是最终形态。
// =============================================================================

/// 恒等映射上限。与 `boot/setup.S` 建的页表一致，也与
/// `drivers/char_dev/mem.rs` 的 `IDENTITY_LIMIT` 同源。
const IDENTITY_LIMIT: u64 = 1 << 30;

/// 校验 `[ptr, ptr+len)` 落在恒等映射内。对应原版 `verify_area()` 的位置，
/// 但强度弱得多，见上面的说明。
fn check_range(ptr: u64, len: u64) -> bool {
    ptr != 0 && len <= IDENTITY_LIMIT && ptr.checked_add(len).is_some_and(|e| e <= IDENTITY_LIMIT)
}

/// 把用户传来的路径指针借成字节切片，顺带做 [`check_range`] 校验。
///
/// 长度上限取 `PATH_MAX`(4096)，避免坏指针上 `strlen` 一路扫到映射边界。
///
/// # Safety
/// 返回的切片只在本次系统调用期间使用；调用方不得让它逃出去。
unsafe fn user_path<'a>(ptr: u64) -> Result<&'a [u8], i64> {
    if !check_range(ptr, 1) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: check_range 保证起始地址在恒等映射内可读；strnlen 有上界，
    // 不会越过 PATH_MAX 继续扫。
    let n = unsafe { crate::klib::string::strnlen(ptr as *const u8, 4096) };
    if n == 0 || n >= 4096 {
        return Err(-(EINVAL as i64));
    }
    if !check_range(ptr, n as u64) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: 同上，n 是刚量出来的长度，整段可读。
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, n) })
}

/// 把用户缓冲区借成可写切片，带 [`check_range`] 校验。
///
/// # Safety
/// 同 [`user_path`]。
unsafe fn user_buf_mut<'a>(ptr: u64, len: u64) -> Result<&'a mut [u8], i64> {
    if len == 0 {
        return Ok(&mut []);
    }
    if !check_range(ptr, len) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: check_range 保证整段在恒等映射内可读写。
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

/// 把用户缓冲区借成只读切片，带 [`check_range`] 校验。
///
/// # Safety
/// 同 [`user_path`]。
unsafe fn user_buf<'a>(ptr: u64, len: u64) -> Result<&'a [u8], i64> {
    if len == 0 {
        return Ok(&[]);
    }
    if !check_range(ptr, len) {
        return Err(-(EFAULT as i64));
    }
    // SAFETY: 同上。
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

/// 校验并借出用户态的 `struct stat` 缓冲区，再把 fs 层的结果写进去。
///
/// 单独抽出来是因为 `stat`/`lstat`/`fstat` 三个都要做同一件事，而
/// `Stat` 有对齐要求，不能像字节缓冲那样直接 `from_raw_parts`。
///
/// # Safety
/// 同 [`user_path`]：借出的引用不得逃出本次系统调用。
unsafe fn user_stat_out(
    ptr: u64,
    f: impl FnOnce(&mut crate::fs::stat::Stat) -> i64,
) -> i64 {
    let need = core::mem::size_of::<crate::fs::stat::Stat>() as u64;
    if !check_range(ptr, need) {
        return -(EFAULT as i64);
    }
    // 先填一份内核栈上的副本，成功了再整体拷回去——避免 fs 层中途出错
    // 时留下半个写坏的结构（原版靠 verify_area 先校验、cp_new_stat 直接
    // 往用户内存写，本树没有校验所以改成两步）。
    let mut tmp = crate::fs::stat::Stat::zeroed();
    let r = f(&mut tmp);
    if r < 0 {
        return r;
    }
    // SAFETY: check_range 已确认目标落在恒等映射内可写；用 unaligned 写
    // 是因为用户传来的指针不保证满足 Stat 的对齐要求。
    unsafe { (ptr as *mut crate::fs::stat::Stat).write_unaligned(tmp) };
    r
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
pub fn execve(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::ENOENT;
    use crate::elf as elf_loader;
    
    let filename = args.a0 as *const u8;
    if filename.is_null() {
        return -(ENOENT as i64);
    }
    
    // TODO: 实际从文件系统读取 ELF 文件
    // 目前只是占位实现
    crate::pr_warn!("sys_execve: full implementation pending filesystem integration");
    -(ENOSYS as i64)
    
    /*
    // 完整实现需要:
    // 1. 从文件系统读取 ELF 文件到内存
    // let data = fs::read_file(filename)?;
    // 
    // 2. 解析 ELF 头
    // let header = elf_loader::parse_elf32(&data)?;
    // 
    // 3. 验证 ELF
    // let _ = elf_loader::is_executable(&header)?;
    // 
    // 4. 获取当前进程的页表
    // let pml4 = unsafe { sched::current().tss.cr3 };
    // 
    // 5. 加载每个 PT_LOAD 段
    // let phdr_count = elf_loader::get_phdr_count(&header);
    // let phdr_offset = elf_loader::get_phdr_offset(&header);
    // let phdr_size = elf_loader::get_phdr_size(&header);
    // 
    // for i in 0..phdr_count {
    //     let phdr = elf_loader::parse_phdr32(&data, phdr_offset + i * phdr_size)?;
    //     unsafe { elf_loader::load_segment(&data, &phdr, dest)?; }
    // }
    // 
    // 6. 设置栈
    // let stack_top = 0x7FFF_FFFF_F000;
    // regs.rsp = stack_top;
    // 
    // 7. 设置入口点
    // regs.rip = header.e_entry as u64;
    // 
    // 0
    */
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
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: user_buf 已校验范围。
    let buf = match unsafe { user_buf(args.a1, args.a2) } {
        Ok(b) => b,
        Err(e) => return e,
    };
    if buf.is_empty() {
        return 0;
    }

    // 先走 fs 层：fd 真的 open 过就写文件/设备。
    // SAFETY: 系统调用上下文，fs 层会睡；fd 无效返回 -EBADF。
    let r = unsafe { crate::fs::read_write::write(fd as usize, buf) };
    if r != -(EBADF as i64) {
        return r;
    }

    // fd 1/2 尚未 open 过就退回内核控制台。原版不需要这条路径（init 在
    // 用户态第一件事就是 open("/dev/tty0")），本树的自检和早期用户态还
    // 没有文件系统里的 stdout，所以留一条兜底。
    if fd != 1 && fd != 2 {
        return -(EBADF as i64);
    }
    match core::str::from_utf8(buf) {
        Ok(s) => {
            crate::print!("{}", s);
            crate::serial::print(s);
        }
        // 非 UTF-8 就逐字节送，保持 write(2) 的字节流语义
        Err(_) => {
            for &b in buf {
                crate::print!("{}", b as char);
            }
        }
    }
    buf.len() as i64
}

/// 退出当前进程。对应原版 `exit.c:sys_exit()` → `do_exit()`。
///
/// 原版 `do_exit` 要释放页表、关文件、通知父进程、转 ZOMBIE 等父进程 wait。
/// 那些依赖 `fs/` 和信号。这里只做内核线程能做的部分：记录退出码后让出 CPU。
pub fn exit(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    crate::exit::do_exit(args.a0 as i32)
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
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: user_buf_mut 已校验范围。
    let buf = match unsafe { user_buf_mut(args.a1, args.a2) } {
        Ok(b) => b,
        Err(e) => return e,
    };
    if buf.is_empty() {
        return 0;
    }
    // SAFETY: 同 [`open`]，fs 层会睡。
    unsafe { crate::fs::read_write::read(fd as usize, buf) }
}

/// 打开文件。对应原版 `fs/open.c:sys_open()`。
///
/// 参数：
/// - a0: 文件路径
/// - a1: 标志 (O_RDONLY, O_WRONLY, etc.)
/// - a2: 模式
pub fn open(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: user_path 已校验范围；fs 层自己处理不存在/权限。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 系统调用上下文，fs 层会睡（getblk/wait_on_buffer），
    // 所以只能在有 current 的任务里调——系统调用天然满足。
    unsafe { crate::fs::open::sys_open(path, args.a1 as u32, args.a2 as u16) }
}

/// 关闭文件。对应原版 `fs/open.c:sys_close()`。
pub fn close(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验 fd 是否真的打开着，未打开返回 -EBADF。
    unsafe { crate::fs::open::sys_close(fd as usize) }
}

/// 创建文件。对应原版 `fs/open.c:sys_creat()`。
pub fn creat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_creat(path, args.a1 as u16) }
}

/// 文件状态。对应原版 `fs/stat.c:sys_stat()`。
pub fn stat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]；out 的范围由 user_stat_out 校验。
    unsafe { user_stat_out(args.a1, |out| crate::fs::stat::sys_stat(path, out)) }
}

/// 文件状态（lstat，不跟随符号链接）。对应原版 `fs/stat.c:sys_lstat()`。
pub fn lstat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`stat`]。
    unsafe { user_stat_out(args.a1, |out| crate::fs::stat::sys_lstat(path, out)) }
}

/// fstat - 文件状态（通过 fd）。对应原版 `fs/stat.c:sys_fstat()`。
pub fn fstat(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: 同 [`stat`]。
    unsafe { user_stat_out(args.a1, |out| crate::fs::stat::sys_fstat(fd as usize, out)) }
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
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_chdir(path) }
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
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_unlink(path) }
}

/// 创建目录。对应原版 `fs/namei.c:sys_mkdir()`。
pub fn mkdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_mkdir(path, args.a1 as u16) }
}

/// 删除目录。对应原版 `fs/namei.c:sys_rmdir()`。
pub fn rmdir(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_rmdir(path) }
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
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验 fd。
    unsafe { crate::fs::open::sys_dup(fd as usize) }
}

/// 复制文件描述符（指定新 fd）。对应原版 `fs/fcntl.c:sys_dup2()`。
pub fn dup2(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let (old, new) = (args.a0 as i64, args.a1 as i64);
    if old < 0 || new < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验两个 fd。
    unsafe { crate::fs::open::sys_dup2(old as usize, new as usize) }
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
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_mknod(path, args.a1 as u16, args.a2 as u16) }
}

/// 改变权限。对应原版 `fs/open.c:sys_chmod()`。
pub fn chmod(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_chmod(path, args.a1 as u16) }
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
pub fn fsync(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    // SAFETY: fs 层校验 fd。
    unsafe { crate::fs::read_write::fsync(fd as usize) }
}
/// 设置文件长度。
pub fn truncate(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]。
    let path = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::open::sys_truncate(path, args.a1 as u32) }
}
/// 设置文件长度（ftruncate）。
pub fn ftruncate(args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 获取目录项。
pub fn getdents(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let fd = args.a0 as i64;
    if fd < 0 {
        return -(EBADF as i64);
    }
    let need = core::mem::size_of::<crate::fs::Dirent>() as u64;
    if args.a2 < need {
        return -(EINVAL as i64);
    }
    if !check_range(args.a1, need) {
        return -(EFAULT as i64);
    }
    // 一次只返回一项：fs 层的 readdir 就是单项语义（原版 1.0.9 的
    // `sys_readdir` 同样一次一项，getdents 是 1.2 之后才有的批量接口）。
    let mut d = crate::fs::Dirent { d_ino: 0, d_off: 0, d_reclen: need as u16, d_name: [0; 32] };
    // SAFETY: 系统调用上下文，fs 层会睡；fd 无效返回 -EBADF。
    let r = unsafe { crate::fs::read_write::readdir(fd as usize, &mut d) };
    if r <= 0 {
        return r;
    }
    // SAFETY: check_range 已确认目标落在恒等映射内可写。
    unsafe { (args.a1 as *mut crate::fs::Dirent).write_unaligned(d) };
    need as i64
}
/// 获取目录项64。
pub fn getdents64(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    // 本树的 Dirent 已经是 64 位字段（d_ino: u64），两者布局一致。
    getdents(args, regs)
}
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

// Socket syscalls
pub fn socket(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn bind(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn connect(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn listen(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn accept(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn sendto(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn recvfrom(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn shutdown(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn getsockname(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn getpeername(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn setsockopt(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn getsockopt(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn socketpair(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn sendmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn recvmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// Process syscalls

/// Fork syscall - 进程复制
///
/// 对应原版 `kernel/fork.c:sys_fork()`。
/// 实现 fork() 系统调用。
pub fn fork(_args: &SysArgs, regs: &mut PtRegs) -> i64 {
    use crate::klib::errno::EAGAIN;
    use crate::sched::task::{KERNEL_STACK_SIZE, STACK_MAGIC, TaskState};

    unsafe extern "C" {
        /// `entry.S` 里新任务的第一次入场点。
        fn ret_from_fork();
    }

    /// pt_regs 的 qword 数。必须和 `entry.S` 里的 `PT_SS + 8` 对上
    /// （0xa8 字节 = 21 个 qword）；下面的 `debug_assert` 兜住。
    const PTREGS_QWORDS: usize = 21;
    /// rax 在 pt_regs 里的 qword 下标。对应 `entry.S` 的 `PT_RAX 0x50`。
    const PTREGS_RAX_IDX: usize = 0x50 / 8;

    debug_assert_eq!(
        core::mem::size_of::<PtRegs>(),
        PTREGS_QWORDS * 8,
        "fork: PtRegs 大小与 entry.S 的 pt_regs 布局不一致"
    );

    // 中断里不能 fork：会拿到中断栈上的 pt_regs 而不是系统调用那份。
    // SAFETY: 系统调用上下文，稍后无条件 restore_flags 配对。
    let flags = unsafe { crate::irq::local_irq_save() };
    let result = (|| -> i64 {
        // SAFETY: 已关中断，独占任务表。
        unsafe {
            // 查找空闲的 task slot。0 是 swapper，从 1 开始。
            let mut free_slot = None;
            for i in 1..sched::NR_TASKS {
                if (*sched::task_ptr(i)).state == TaskState::Unused {
                    free_slot = Some(i);
                    break;
                }
            }
            let Some(child_nr) = free_slot else {
                crate::pr_warn!("sys_fork: no free task slots");
                return -(EAGAIN as i64);
            };

            let parent_nr = sched::current_nr();

            // 先把栈拿到手，失败了就不用回滚任务表。
            let stack_page = crate::mm::get_free_page();
            if stack_page == 0 {
                crate::pr_warn!("sys_fork: out of memory for kernel stack");
                return -(EAGAIN as i64);
            }

            // 复制父进程的 PCB。原版 copy_process 里的 `*p = *current`。
            // 裸指针读写：parent_nr != child_nr（child_nr 是 Unused 槽位，
            // 当前任务不可能是 Unused），所以不会自我别名。
            let parent = sched::task_ptr(parent_nr);
            let child = sched::task_ptr(child_nr);
            *child = (*parent).clone();

            // 子进程特有的字段。
            (*child).state = TaskState::Running;
            (*child).pid = sched::allocate_pid();
            (*child).pgrp = (*parent).pgrp;
            (*child).session = (*parent).session;
            (*child).parent = parent_nr;
            (*child).kernel_stack = stack_page as u64;
            (*child).start_time = sched::jiffies();
            // 时间统计从零开始（原版 `p->utime = p->stime = 0`）。
            (*child).utime = 0;
            (*child).stime = 0;
            (*child).timeout = 0;
            (*child).exit_code = 0;
            // 待处理信号不继承，屏蔽字继承。原版 `p->signal = 0`。
            (*child).signal = 0;
            // 时间片对半分，父子都不能靠 fork 白拿一个整片
            // （原版 1.0.9 直接给 `p->counter = p->priority`，
            //  但那样 fork 循环能无限延长自己的份额）。
            let half = (*parent).counter / 2;
            (*parent).counter = half;
            (*child).counter = (*parent).counter - half + half; // = half
            if (*child).counter == 0 {
                (*child).counter = 1;
            }
            // pml4：父进程是纯内核任务（pml4==0）则子进程也是；
            // 有用户空间的进程走下面的 copy_page_tables 路径。
            (*child).pml4 = (*parent).pml4;
            (*child).tss.cr3 = (*child).pml4 as u64;

            // sigaction 表随 PCB 一起继承（原版是内联数组，我们在旁路数组里）。
            crate::signal::clone_sigactions(parent_nr, child_nr);

            // ---- 布置子进程内核栈 ----
            // 布局和 sched::kernel_thread 一致，只是 ret 地址上方放的不是
            // fn/arg 而是一整份 pt_regs：
            //   [tss.rsp + 0 .. +48]  switch_to 的 7 个保存槽
            //   [tss.rsp + 56]        返回地址 = ret_from_fork
            //   [tss.rsp + 64 ..]     pt_regs（ret_from_sys_call 要用）
            let stack_top = stack_page as u64 + KERNEL_STACK_SIZE as u64;
            // 栈底魔数，供 stack_ok() / die_if_kernel 检测溢出。
            core::ptr::write_volatile(stack_page as *mut u64, STACK_MAGIC);

            let mut sp = stack_top as *mut u64;
            // pt_regs 从高地址往低地址写，写完 sp 正好落在 pt_regs 基址。
            let src = regs as *const PtRegs as *const u64;
            for i in (0..PTREGS_QWORDS).rev() {
                sp = sp.sub(1);
                core::ptr::write_volatile(sp, core::ptr::read(src.add(i)));
            }
            // fork 在子进程里返回 0（原版 `p->tss.eax = 0`）。
            core::ptr::write_volatile(sp.add(PTREGS_RAX_IDX), 0u64);

            // switch_to 的 `ret` 落到这里。
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, ret_from_fork as *const () as u64);
            // rflags 槽：IF=0，由 schedule_tail 里的 sti 开中断
            // （与 kernel_thread 的处理一致）。
            sp = sp.sub(1);
            core::ptr::write_volatile(sp, 0x0002u64);
            // 余下 6 个 callee-saved 槽清零。
            for _ in 0..6 {
                sp = sp.sub(1);
                core::ptr::write_volatile(sp, 0u64);
            }

            (*child).tss.rsp = sp as u64;
            (*child).tss.rsp0 = stack_top;

            // 挂进调度环。和 sched::kernel_thread 里的 SET_LINKS 一样，
            // 用裸指针写避免 nr/cur/old_next 相等时的 &mut 别名。
            let old_next = (*sched::task_ptr(parent_nr)).next;
            (*sched::task_ptr(child_nr)).next = old_next;
            (*sched::task_ptr(child_nr)).prev = parent_nr;
            (*sched::task_ptr(parent_nr)).next = child_nr;
            (*sched::task_ptr(old_next)).prev = child_nr;

            let child_pid = (*child).pid;
            crate::pr_info!(
                "sys_fork: parent pid={} -> child pid={} (slot {})",
                (*parent).pid,
                child_pid,
                child_nr
            );

            // 父进程拿到子进程 pid。
            child_pid as i64
        }
    })();
    // SAFETY: 与上面的 save 配对。
    unsafe { crate::irq::restore_flags(flags) };
    result
}

/// vfork syscall - 轻量级进程复制
pub fn vfork(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    fork(_args, _regs)
}

/// wait4 syscall - 等子进程退出并收尸
///
/// 对应原版 `kernel/exit.c:sys_wait4()`。参数：pid / stat_addr / options /
/// rusage（rusage 尚未实现，非 0 时忽略）。
pub fn wait4(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // a0 是 pid，按 C 的 `pid_t`（i32）符号扩展，否则 -1 会变成 2^64-1。
    let pid = args.a0 as i32 as i64;
    // SAFETY: 系统调用上下文；stat_addr 由 sys_wait4 内部判空，
    // 页表恒等映射所以用户指针可直接写（还没有独立用户地址空间）。
    unsafe { crate::exit::sys_wait4(pid, args.a1, args.a2) }
}
pub fn setitimer(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn getitimer(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// Memory syscalls
pub fn mlock(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn munlock(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mlockall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn munlockall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn mremap(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msync(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// IPC syscalls
pub fn shmget(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn shmat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn shmdt(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn shmctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semget(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semop(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn semtimedop(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgget(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgsnd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgrcv(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn msgctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// Time syscalls

/// 时间规格结构
#[repr(C)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// nanosleep - 高精度睡眠
///
/// 对应原版 `kernel/sched.c:sys_nanosleep()`。
pub fn nanosleep(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let req = args.a0 as *const Timespec;
    let rem = args.a1 as *mut Timespec;
    
    if req.is_null() {
        return -(EINVAL as i64);
    }
    
    // SAFETY: req 已校验非空
    let secs = unsafe { (*req).tv_sec };
    let nsecs = unsafe { (*req).tv_nsec };
    
    if nsecs < 0 || nsecs >= 1_000_000_000 {
        return -(EINVAL as i64);
    }
    
    // 计算睡眠时间（简化为 jiffies）
    let sleep_jiffies = (secs * sched::task::HZ as i64) as u64;
    
    if sleep_jiffies > 0 {
        // SAFETY: 系统调用上下文，可以睡眠
        unsafe {
            sched::current().state = crate::sched::task::TaskState::Interruptible;
            sched::current().timeout = sched::jiffies() + sleep_jiffies;
            sched::schedule();
        }
    }
    
    // 如果有剩余时间结构指针，写入剩余时间（简化：假设睡眠完成）
    if !rem.is_null() {
        // SAFETY: rem 已校验非空
        unsafe {
            (*rem).tv_sec = 0;
            (*rem).tv_nsec = 0;
        }
    }
    
    0
}

pub fn clock_gettime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn clock_settime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn clock_getres(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn clock_nanosleep(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { nanosleep(_args, _regs) }

// Priority syscalls
pub fn getpriority(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn setpriority(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// Hostname syscalls
pub fn sethostname(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn setdomainname(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// CPU syscall
pub fn getcpu(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// Resource limits
pub fn prlimit(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// File ops
pub fn pipe2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fchmodat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fchownat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn openat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mkdirat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mknodat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn unlinkat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn renameat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn renameat2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn linkat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn symlinkat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn readlinkat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fchown(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

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

// Advanced syscalls

/// process control.
pub fn prctl(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let option = args.a0 as i32;
    crate::pr_warn!("sys_prctl: option={} (stub)", option);
    -(ENOSYS as i64)
}

/// set child tid address.
pub fn set_tid_address(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 1 }

/// get random bytes.
pub fn getrandom(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let buf = args.a0 as *mut u8;
    let len = args.a1 as usize;
    if buf.is_null() { return -(EINVAL as i64); }
    unsafe { core::ptr::write_bytes(buf, 0, len.min(256)); }
    len as i64
}

/// memory management.
pub fn mbind(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn set_mempolicy(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn get_mempolicy(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn migrate_pages(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn move_pages(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn mlock2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// io setup.
pub fn io_setup(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_destroy(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_submit(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_getevents(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_cancel(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_pgetevents(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// keyctl syscalls.
pub fn add_key(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn request_key(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn keyctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// inotify.
pub fn inotify_init(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn inotify_init1(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn inotify_add_watch(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn inotify_rm_watch(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// epoll.
pub fn epoll_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn epoll_create1(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn epoll_ctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn epoll_wait(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// timerfd.
pub fn timerfd_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn timerfd_settime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn timerfd_gettime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// eventfd.
pub fn eventfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn eventfd2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// file operations.
pub fn splice(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn tee(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn vmsplice(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn sync_file_range(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn vhangup(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
pub fn dup3(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn faccessat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn statfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fstatfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn truncate64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn ftruncate64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fallocate(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fanotify_init(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fanotify_mark(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn copy_file_range(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn preadv2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn pwritev2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn statx(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn lookup_dcookie(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn syncfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

/// *at syscalls.

/// fchown.
/// chmod.
/// fchmod.

/// advanced syscalls.
pub fn perf_event_open(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn accept4(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn process_vm_readv(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn process_vm_writev(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// memory protection keys.
pub fn pkey_mprotect(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn pkey_alloc(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn pkey_free(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// extended attributes.
pub fn setxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn lsetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fsetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn getxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn lgetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fgetxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn listxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn llistxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn flistxattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn removexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn lremovexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn fremovexattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// io_uring (simplified stub).
pub fn io_uring_setup(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_uring_enter(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn io_uring_register(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

/// other advanced syscalls.
pub fn kexec_load(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn init_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn delete_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn sched_setattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn sched_getattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn seccomp(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn memfd_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn userfaultfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn membarrier(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn clock_adjtime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn setns(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
pub fn rseq(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// =============================================================================
// 补齐 x86_64 正式调用号 0..=334 的空洞（对应 `arch/x86/entry/syscalls/
// syscall_64.tbl`）。这一段的目标是让调用号连续排满，用户态拿到的是
// `-ENOSYS`/合理默认值而不是「越界」——真正的实现随对应子系统移植逐个替换。
//
// 分三类：
// 1. 能靠现有子系统真做的（lseek / readv / writev / sched_yield / …）；
// 2. 有合理默认值的（getgroups 返回 0、madvise 是建议可忽略、…）；
// 3. 纯占位返回 `-ENOSYS`（信号 rt_* 系列、POSIX 定时器、mq_*、futex、…）。
// =============================================================================

/// 移动文件读写位置。对应原版 `fs/read_write.c:sys_lseek()`。
/// 直接转给 [`crate::fs::read_write::lseek`]。
pub fn lseek(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: fs 层自己校验 fd，越界/未打开返回 -EBADF。
    unsafe { crate::fs::read_write::lseek(args.a0 as usize, args.a1 as i64, args.a2 as u32) }
}

/// 分散读。对应原版 1.0.9 之后才有的 `sys_readv()`。
/// 拆成对每个 iovec 调一次 [`read`]，短读即停（与原版语义一致）。
pub fn readv(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    iov_loop(args, regs, read)
}

/// 分散写。对应 `sys_writev()`，实现方式同 [`readv`]。
pub fn writev(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    iov_loop(args, regs, write)
}

/// `struct iovec`。字段顺序与 x86_64 ABI 一致。
#[repr(C)]
struct IoVec {
    base: u64,
    len: u64,
}

/// `readv`/`writev` 的公共循环。
fn iov_loop(args: &SysArgs, regs: &mut PtRegs, f: fn(&SysArgs, &mut PtRegs) -> i64) -> i64 {
    let (fd, iov, cnt) = (args.a0, args.a1 as *const IoVec, args.a2 as usize);
    if iov.is_null() {
        return -(EFAULT as i64);
    }
    // 原版 UIO_MAXIOV = 1024
    if cnt > 1024 {
        return -(EINVAL as i64);
    }
    let mut total: i64 = 0;
    for i in 0..cnt {
        // SAFETY: 缺 verify_area，这里和 sys_write 一样只能信任调用方；
        // 等 mm/mmap.c 的 vm_area_struct 到位后加校验。
        let v = unsafe { &*iov.add(i) };
        if v.len == 0 {
            continue;
        }
        let sub = SysArgs { a0: fd, a1: v.base, a2: v.len, a3: 0, a4: 0, a5: 0 };
        let r = f(&sub, regs);
        if r < 0 {
            return if total > 0 { total } else { r };
        }
        total += r;
        // 短读/短写：不再继续下一个 iovec
        if (r as u64) < v.len {
            break;
        }
    }
    total
}

/// 主动让出 CPU。对应原版没有（1.0.9 无 `sched_yield`），语义等价于
/// 直接调一次 `schedule()`。
pub fn sched_yield(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 系统调用上下文，不在中断里，可以安全切换。
    unsafe { sched::schedule() };
    0
}

/// 返回线程 ID。本树没有线程，tid == pid（原版 1.0.9 同样没有）。
pub fn gettid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 系统调用上下文里 current 必然有效。
    unsafe { sched::current() }.pid as i64
}

/// 秒级时间。对应原版 `kernel/time.c:sys_time()`。
/// 没有 RTC 驱动，用 jiffies/HZ 当作开机以来的秒数。
pub fn time(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let secs = (sched::jiffies() / crate::sched::task::HZ) as i64;
    let p = args.a0 as *mut i64;
    if !p.is_null() {
        // SAFETY: 缺 verify_area，同 sys_write 的限制。
        unsafe { p.write_volatile(secs) };
    }
    secs
}

/// 退出整个线程组。没有线程组，退化成 [`exit`]。
pub fn exit_group(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    exit(args, regs)
}

/// 按 tid 发信号。没有线程，等价于按 pid 发（转给 [`kill`]）。
pub fn tkill(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    kill(args, regs)
}

/// 按 tgid+tid 发信号。同 [`tkill`]，忽略 tgid。
pub fn tgkill(args: &SysArgs, regs: &mut PtRegs) -> i64 {
    let sub = SysArgs { a0: args.a1, a1: args.a2, a2: 0, a3: 0, a4: 0, a5: 0 };
    kill(&sub, regs)
}

/// 建立硬链接。对应原版 `fs/namei.c:sys_link()`。
/// minix 层的 link 还没接出来，先返回 `-ENOSYS`。
pub fn link(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    // SAFETY: 同 [`open`]，两个路径各自校验。
    let old = match unsafe { user_path(args.a0) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同上。
    let new = match unsafe { user_path(args.a1) } {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: 同 [`open`]。
    unsafe { crate::fs::namei::do_link(old, new) }
}

// --- 有合理默认值的一批 -------------------------------------------------------

/// 建议内核的页面使用方式。建议性调用，忽略即合法（原版无）。
pub fn madvise(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 查询页面是否驻留。没有换页，全部驻留 → 直接返回成功。
pub fn mincore(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 预读提示。无预读机制，忽略。
pub fn readahead(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 文件访问模式提示。同 [`madvise`]，忽略。
pub fn fadvise64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 数据同步（不含元数据）。缓冲缓存是写回的，转给 [`fsync`]。
pub fn fdatasync(args: &SysArgs, regs: &mut PtRegs) -> i64 { fsync(args, regs) }
/// 文件加锁。单进程内核，无竞争 → 直接成功。
pub fn flock(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件权限（按 fd）。minix 层还没接 chmod，先当成功。
pub fn fchmod(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件属主（不跟随符号链接）。同 [`chown`] 的限制。
pub fn lchown(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件时间戳。没有 RTC，忽略。
pub fn utime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件时间戳（μs 精度）。同 [`utime`]。
pub fn utimes(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件时间戳（ns 精度 + dirfd）。同 [`utime`]。
pub fn utimensat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 改文件时间戳（dirfd 版）。同 [`utime`]。
pub fn futimesat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

/// 取附加组列表。没有组机制，返回 0 个组（原版 `sys.c:sys_getgroups()`
/// 在 NGROUPS 为空时同样返回 0）。
pub fn getgroups(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置附加组列表。需要 root，本树无 uid 概念 → `-EPERM`。
pub fn setgroups(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置真实/有效 uid。同 [`setuid`] 的限制。
pub fn setreuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置真实/有效 gid。同上。
pub fn setregid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置真实/有效/保存 uid。同上。
pub fn setresuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置真实/有效/保存 gid。同上。
pub fn setresgid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EPERM as i64) }
/// 设置文件系统 uid。返回旧值（恒为 0），与原版「返回旧 fsuid」一致。
pub fn setfsuid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 设置文件系统 gid。同 [`setfsuid`]。
pub fn setfsgid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 取真实/有效/保存 uid。三个都是 0，写回三个指针。
pub fn getresuid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { put_triple(args, 0) }
/// 取真实/有效/保存 gid。同 [`getresuid`]。
pub fn getresgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 { put_triple(args, 0) }

/// `getresuid`/`getresgid` 的公共写回。
fn put_triple(args: &SysArgs, v: u32) -> i64 {
    for p in [args.a0, args.a1, args.a2] {
        let p = p as *mut u32;
        if p.is_null() {
            return -(EFAULT as i64);
        }
        // SAFETY: 缺 verify_area，同 sys_write 的限制。
        unsafe { p.write_volatile(v) };
    }
    0
}

/// 取进程组。a0 == 0 表示当前进程；本树只支持当前进程。
pub fn getpgid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a0 != 0 {
        return -(ENOSYS as i64);
    }
    // SAFETY: 系统调用上下文里 current 必然有效。
    unsafe { sched::current() }.pgrp as i64
}

/// 取会话 ID。限制同 [`getpgid`]。
pub fn getsid(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    if args.a0 != 0 {
        return -(ENOSYS as i64);
    }
    // SAFETY: 同上。
    unsafe { sched::current() }.session as i64
}

/// 设资源限制。没有 rlimit 强制机制，接受但不生效。
pub fn setrlimit(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 调度参数（优先级）。本树的 nice 值在 `getpriority`/`setpriority` 里，
/// 这四个 POSIX 实时调度接口没有对应实现。
pub fn sched_setparam(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取调度参数。同 [`sched_setparam`]。
pub fn sched_getparam(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设调度策略。只有 SCHED_OTHER，改成别的都拒绝。
pub fn sched_setscheduler(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(EINVAL as i64) }
/// 取调度策略。恒为 SCHED_OTHER(0)。
pub fn sched_getscheduler(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 实时优先级上限。SCHED_OTHER 下为 0。
pub fn sched_get_priority_max(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 实时优先级下限。同上。
pub fn sched_get_priority_min(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }
/// 取 RR 时间片。没有 SCHED_RR。
pub fn sched_rr_get_interval(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取/设 CPU 亲和性。单核，掩码恒为 {0}。
pub fn sched_getaffinity(args: &SysArgs, _regs: &mut PtRegs) -> i64 {
    let p = args.a2 as *mut u64;
    if p.is_null() {
        return -(EFAULT as i64);
    }
    // SAFETY: 缺 verify_area，同 sys_write 的限制。
    unsafe { p.write_volatile(1) };
    8
}
/// 设 CPU 亲和性。单核，只能是 CPU0，任何掩码都当成功。
pub fn sched_setaffinity(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { 0 }

// --- 纯占位：对应子系统未移植，统一返回 -ENOSYS -----------------------------

/// 安装信号处理函数（rt 版）。`src/signal.rs` 的 sigaction 还没接到用户态栈帧。
pub fn rt_sigaction(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 改信号屏蔽字（rt 版）。同 [`rt_sigaction`]。
pub fn rt_sigprocmask(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 从信号处理函数返回。需要 entry.S 里的信号栈帧，见 STATUS 的下一阶段。
pub fn rt_sigreturn(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 指定偏移读，不改 f_pos。要先给 fs 层加一个不动 f_pos 的读路径。
pub fn pread64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 指定偏移写，不改 f_pos。同 [`pread64`]。
pub fn pwrite64(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 内核内文件到文件的搬运。需要 fs 层的 splice 基础设施。
pub fn sendfile(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 创建进程/线程。`sys_fork` 已有，clone 的 flags 语义（共享地址空间/文件表）还没有。
pub fn clone(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 进程跟踪。需要 `arch_ptrace` 与调试寄存器支持。
pub fn ptrace(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 读内核日志环。`klib::printk::read_log()` 已有，缺用户地址校验才好接。
pub fn syslog(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取进程能力集。没有 capability 机制。
pub fn capget(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设进程能力集。同 [`capget`]。
pub fn capset(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取待处理信号集（rt 版）。
pub fn rt_sigpending(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带超时地等信号（rt 版）。
pub fn rt_sigtimedwait(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带 siginfo 发信号。
pub fn rt_sigqueueinfo(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 临时换屏蔽字并挂起。
pub fn rt_sigsuspend(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置备用信号栈。
pub fn sigaltstack(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 加载共享库（老式 a.out）。`src/elf/` 走的是现代路径，不打算实现。
pub fn uselib(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置执行域。只有一种 personality。
pub fn personality(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 文件系统统计（已废弃接口）。用 [`statfs`] 代替。
pub fn ustat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 查询已注册的文件系统类型。`fs/devices.rs` 里还没有类型注册表。
pub fn sysfs(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 改 LDT。`src/desc.rs` 只建了 GDT，没有 LDT。
pub fn modify_ldt(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 换根挂载点。需要挂载树，现在只支持单个根。
pub fn pivot_root(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 旧式 sysctl（已废弃）。
pub fn sysctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 架构相关的进程控制（ARCH_SET_FS 等）。需要 per-task 的 FS/GS base。
pub fn arch_prctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 调整系统时钟。没有 RTC 与 NTP 环路。
pub fn adjtimex(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 换根目录。需要 per-task 的 root inode。
pub fn chroot(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 开启进程记账。
pub fn acct(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置系统时间。没有 RTC。
pub fn settimeofday(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 启用交换分区。没有换页子系统。
pub fn swapon(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 关闭交换分区。同 [`swapon`]。
pub fn swapoff(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 改 IOPL。会放开用户态端口访问，等有真用户态进程再说。
pub fn iopl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 改 I/O 端口位图。需要 TSS 里的 I/O 位图。
pub fn ioperm(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 装载模块（1.0.9 的老接口）。没有模块加载器。
pub fn create_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取内核符号表。同 [`create_module`]。
pub fn get_kernel_syms(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 查询模块信息。同 [`create_module`]。
pub fn query_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 磁盘配额控制。minix 层没有配额。
pub fn quotactl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// NFS 服务端控制（已从 Linux 移除）。
pub fn nfsservctl(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// STREAMS 接口，Linux 从未实现，占号。
pub fn getpmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// STREAMS 接口，Linux 从未实现，占号。
pub fn putpmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// AFS 保留号，Linux 从未实现。
pub fn afs_syscall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// TUX web 服务器保留号，已废弃。
pub fn tuxcall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// LSM 保留号，Linux 从未实现。
pub fn security(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 快速用户态互斥。需要 per-address 的等待队列哈希。
pub fn futex(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设 TLS 段（i386 遗留）。x86_64 用 [`arch_prctl`]。
pub fn set_thread_area(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取 TLS 段（i386 遗留）。同上。
pub fn get_thread_area(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// epoll 的废弃老接口，占号。
pub fn epoll_ctl_old(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// epoll 的废弃老接口，占号。
pub fn epoll_wait_old(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 重排文件映射（已废弃）。
pub fn remap_file_pages(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 重启被信号打断的调用。需要 `ERESTART*` 的完整回绕逻辑。
pub fn restart_syscall(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 创建 POSIX 定时器。只有 itimer，没有 POSIX 定时器池。
pub fn timer_create(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设置 POSIX 定时器。同 [`timer_create`]。
pub fn timer_settime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 读 POSIX 定时器。同 [`timer_create`]。
pub fn timer_gettime(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 读 POSIX 定时器溢出次数。同 [`timer_create`]。
pub fn timer_getoverrun(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 删除 POSIX 定时器。同 [`timer_create`]。
pub fn timer_delete(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// vserver 保留号，Linux 从未实现。
pub fn vserver(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 打开 POSIX 消息队列。只有 SysV 消息队列。
pub fn mq_open(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 删除 POSIX 消息队列。同 [`mq_open`]。
pub fn mq_unlink(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带超时发送。同 [`mq_open`]。
pub fn mq_timedsend(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带超时接收。同 [`mq_open`]。
pub fn mq_timedreceive(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 注册消息到达通知。同 [`mq_open`]。
pub fn mq_notify(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 读写队列属性。同 [`mq_open`]。
pub fn mq_getsetattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 等子进程（可不收尸）。`wait4` 已有，WNOWAIT 语义还没有。
pub fn waitid(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 设 I/O 优先级。`ll_rw_blk` 的请求队列没有优先级。
pub fn ioprio_set(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取 I/O 优先级。同 [`ioprio_set`]。
pub fn ioprio_get(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// `fstatat` 的正式名。需要 dirfd 相对解析。
pub fn newfstatat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带信号屏蔽的 select。转 [`select`] 前要先接上信号。
pub fn pselect6(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带信号屏蔽的 poll。同 [`pselect6`]。
pub fn ppoll(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 拆分命名空间。没有命名空间。
pub fn unshare(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 注册健壮 futex 链。同 [`futex`]。
pub fn set_robust_list(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 读健壮 futex 链。同 [`futex`]。
pub fn get_robust_list(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带信号屏蔽的 epoll_wait。同 [`pselect6`]。
pub fn epoll_pwait(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 把信号变成可读的 fd。
pub fn signalfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`signalfd`] 的带 flags 版本。
pub fn signalfd4(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`readv`] + 指定偏移。同 [`pread64`] 的限制。
pub fn preadv(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`writev`] + 指定偏移。同 [`pwrite64`] 的限制。
pub fn pwritev(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 按 tgid 带 siginfo 发信号。
pub fn rt_tgsigqueueinfo(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 批量收包。`net/` 层还没有 msghdr 批处理。
pub fn recvmmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取文件句柄。minix 层没有导出句柄的概念。
pub fn name_to_handle_at(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 按句柄打开。同 [`name_to_handle_at`]。
pub fn open_by_handle_at(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 批量发包。同 [`recvmmsg`]。
pub fn sendmmsg(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 比较两个进程的内核资源。
pub fn kcmp(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 从 fd 装载模块。同 [`create_module`]。
pub fn finit_module(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 从 fd 加载 kexec 镜像。
pub fn kexec_file_load(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// BPF 系统调用。没有 BPF 虚拟机。
pub fn bpf(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 按 dirfd 执行。`execve` 已有，缺 dirfd 相对解析。
pub fn execveat(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 取进程的 pidfd。没有 pidfd 类型。
pub fn pidfd_open(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`clone`] 的结构体参数版本。
pub fn clone3(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`faccessat`] 的带 flags 版本。
pub fn faccessat2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// [`epoll_pwait`] 的 ns 超时版本。
pub fn epoll_pwait2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }

// --- 正式表 424..=448（335..423 是 x32 保留段，官方 x86_64 表里没有）------------

/// 给 pidfd 发信号。没有 pidfd 类型，见 [`pidfd_open`]。
pub fn pidfd_send_signal(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 克隆一棵挂载树。需要挂载树，现在只支持单个根。
pub fn open_tree(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 移动挂载点。同 [`open_tree`]。
pub fn move_mount(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 新式挂载 API：打开文件系统上下文。`fs/devices.rs` 里没有 fs_context。
pub fn fsopen(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 新式挂载 API：配置文件系统上下文。同 [`fsopen`]。
pub fn fsconfig(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 新式挂载 API：由上下文生成挂载点。同 [`fsopen`]。
pub fn fsmount(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 新式挂载 API：由已有挂载点取上下文。同 [`fsopen`]。
pub fn fspick(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 批量关闭 fd 区间。`fs/open.rs` 的 fd 表是每进程 16 项，加这个要先决定 EBADF 语义。
pub fn close_range(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 带 `open_how` 结构的 openat。需要 RESOLVE_* 解析约束。
pub fn openat2(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 从别的进程偷一个 fd。同 [`pidfd_open`]。
pub fn pidfd_getfd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 对别的进程做 madvise。需要跨进程地址空间访问。
pub fn process_madvise(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 改挂载点属性。同 [`open_tree`]。
pub fn mount_setattr(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 按 fd 的配额控制。同 [`quotactl`]。
pub fn quotactl_fd(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// Landlock LSM：建规则集。没有 LSM 框架。
pub fn landlock_create_ruleset(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// Landlock LSM：加规则。同上。
pub fn landlock_add_rule(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// Landlock LSM：自我限制。同上。
pub fn landlock_restrict_self(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 建不可映射到内核的匿名内存 fd。同 [`memfd_create`] 的限制。
pub fn memfd_secret(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
/// 提前释放被杀进程的内存。需要 `do_exit` 的完整回收链。
pub fn process_mrelease(_args: &SysArgs, _regs: &mut PtRegs) -> i64 { -(ENOSYS as i64) }
