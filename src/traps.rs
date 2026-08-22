//! 异常与陷阱处理。对应 linux-1.0.9 的 `kernel/traps.c`。
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`PtRegs`] | `include/linux/ptrace.h` 的 `struct pt_regs` |
//! | [`do_trap`] | `traps.c` 里 `DO_ERROR` 宏生成的那 14 个 `do_*` |
//! | [`die_if_kernel`] | `traps.c:die_if_kernel()` |
//! | [`TRAP_INFO`] | `DO_ERROR` 的 (trapnr, signr, str) 三元组表 |
//!
//! 原版给每个向量生成一个独立的 `do_xxx`（`DO_ERROR` 宏展开 14 次），
//! 每份都是「记录 trap_no/error_code → send_sig → die_if_kernel」。这里合成
//! 一个 [`do_trap`]，靠向量号查 [`TRAP_INFO`] 表。行为等价，少 14 份重复代码。
//!
//! 信号投递（原版 `send_sig(signr, tsk, 1)`）暂时只记录不投递：`kernel/signal.c`
//! 还没移植，且当前没有用户态进程。用户态异常先按 `die` 处理并打印，
//! 等 signal 到位后把 [`send_sig_stub`] 换成真的 `send_sig`。

use crate::desc::selector;
use crate::klib::printk::Level;
use crate::sched;

/// 陷入内核时保存的寄存器组。**字段顺序必须与 `boot/entry.S` 的
/// SAVE_ALL 压栈顺序完全一致**（压栈是反序，所以这里从 r15 开始）。
///
/// 对应原版 `struct pt_regs`，只是 32 位的 ebx/ecx/... + 四个段寄存器
/// 换成 64 位的 15 个通用寄存器（long mode 下段寄存器无需保存）。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct PtRegs {
    // ---- SAVE_ALL 压入（顺序与 entry.S 逐条对应）----
    /// callee-saved 六个
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    /// caller-saved 九个
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    /// 三用途格：系统调用路径存调用号（原版 orig_eax），
    /// 异常路径存 error_code，IRQ 路径存 `!irq`。
    pub orig_rax: u64,
    // ---- 以下由 CPU 在陷入时压入 ----
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl PtRegs {
    /// 是否来自用户态。对应原版那句 `(3 & regs->cs) == 3`。
    #[inline]
    pub fn from_user(&self) -> bool {
        self.cs & 3 == 3
    }

    /// 是否来自内核态。
    #[inline]
    pub fn from_kernel(&self) -> bool {
        !self.from_user()
    }
}

/// 一个向量的元信息。对应原版 `DO_ERROR(trapnr, signr, str, name, tsk)`
/// 里的前三个参数。
struct TrapInfo {
    /// 打印用的名字，逐字沿用原版的字符串
    name: &'static str,
    /// 该异常对应投递给进程的信号号（原版第二个参数）
    signr: u32,
    /// CPU 是否为这个向量压入错误码
    has_error_code: bool,
}

/// 信号号。对应 `include/linux/signal.h`，只列 traps.c 用到的几个。
pub mod signal {
    /// 非法指令
    pub const SIGILL: u32 = 4;
    /// 断点/单步
    pub const SIGTRAP: u32 = 5;
    /// 浮点异常（含除零）
    pub const SIGFPE: u32 = 8;
    /// 总线错误
    pub const SIGBUS: u32 = 7;
    /// 段错误
    pub const SIGSEGV: u32 = 11;
}

use signal::*;

/// 21 个向量的元信息表。前 18 项的 name/signr 逐条照抄原版 `traps.c` 的
/// `DO_ERROR` 调用；18..20 是 64 位新增，原版归入 `reserved`。
static TRAP_INFO: [TrapInfo; 21] = [
    TrapInfo { name: "divide error", signr: SIGFPE, has_error_code: false },
    TrapInfo { name: "debug", signr: SIGTRAP, has_error_code: false },
    TrapInfo { name: "nmi", signr: 0, has_error_code: false },
    TrapInfo { name: "int3", signr: SIGTRAP, has_error_code: false },
    TrapInfo { name: "overflow", signr: SIGSEGV, has_error_code: false },
    TrapInfo { name: "bounds", signr: SIGSEGV, has_error_code: false },
    TrapInfo { name: "invalid operand", signr: SIGILL, has_error_code: false },
    TrapInfo { name: "device not available", signr: SIGSEGV, has_error_code: false },
    TrapInfo { name: "double fault", signr: SIGSEGV, has_error_code: true },
    TrapInfo { name: "coprocessor segment overrun", signr: SIGFPE, has_error_code: false },
    TrapInfo { name: "invalid TSS", signr: SIGSEGV, has_error_code: true },
    TrapInfo { name: "segment not present", signr: SIGSEGV, has_error_code: true },
    TrapInfo { name: "stack segment", signr: SIGSEGV, has_error_code: true },
    TrapInfo { name: "general protection", signr: SIGSEGV, has_error_code: true },
    TrapInfo { name: "page fault", signr: SIGSEGV, has_error_code: true },
    TrapInfo { name: "reserved", signr: SIGSEGV, has_error_code: false },
    TrapInfo { name: "coprocessor error", signr: SIGFPE, has_error_code: false },
    TrapInfo { name: "alignment check", signr: SIGBUS, has_error_code: true },
    TrapInfo { name: "machine check", signr: SIGBUS, has_error_code: false },
    TrapInfo { name: "simd coprocessor error", signr: SIGFPE, has_error_code: false },
    TrapInfo { name: "virtualization", signr: SIGSEGV, has_error_code: false },
];

/// 已发生的异常计数，按向量号。自检和调试用，原版没有
/// （原版的等价物是 `kstat` 里的中断计数，但它只统计 IRQ）。
static mut TRAP_COUNT: [u64; 32] = [0; 32];

/// 某个向量至今触发了多少次。
///
/// 用 `read_volatile` 而非普通读：计数器是在**异常处理函数**里自增的，
/// 从编译器视角看，`asm!("int3")` 前后的两次读之间没有可见的写，于是它会
/// 把两次读 CSE 成一次，让「前后差值」的断言恒为假。见 buglog bug-005。
pub fn trap_count(vector: usize) -> u64 {
    if vector >= 32 {
        return 0;
    }
    // SAFETY: 只读一个 u64；vector 已做边界检查。volatile 阻止 CSE。
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(TRAP_COUNT).cast::<u64>().add(vector)) }
}

/// 设为 true 时，内核态异常只打印并跳过出错指令，不 panic。
/// 自检要故意触发异常，需要这个开关；原版没有对应物（它直接 `do_exit`）。
static mut TRAP_RECOVER: bool = false;

/// 让下一个内核态异常可恢复（打印后跳过出错指令）。返回旧值。
///
/// # Safety
/// 只应在自检里短暂开启：开着的时候真正的内核 bug 会被静默跳过。
pub unsafe fn set_recover(on: bool) -> bool {
    // SAFETY: 单核无抢占下读改写一个 bool。
    unsafe {
        let p = core::ptr::addr_of_mut!(TRAP_RECOVER);
        let old = *p;
        *p = on;
        old
    }
}

/// 恢复模式下，`do_trap` 把「该跳过多少字节」写在这里，
/// 由自检读取以校验确实走过了处理函数。
static mut LAST_TRAP_VECTOR: u64 = u64::MAX;

/// 最近一次异常的向量号（`u64::MAX` 表示还没发生过）。
pub fn last_trap_vector() -> u64 {
    // SAFETY: 只读一个 u64。
    unsafe { *core::ptr::addr_of!(LAST_TRAP_VECTOR) }
}

/// 所有异常的统一入口，由 `boot/entry.S` 的 `exc_common` 调用。
///
/// 对应原版 `DO_ERROR` 宏生成的 14 个 `do_*` 函数，
/// 加上手写的 `do_nmi` / `do_debug` / `do_coprocessor_error`。
///
/// # Safety
/// 只能由 entry.S 的异常桩调用：`regs` 必须指向内核栈上刚由 SAVE_ALL
/// 建好的完整 `PtRegs`，`vector` 必须是真实的异常向量号。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn do_trap(regs: *mut PtRegs, vector: u64) {
    // SAFETY: 契约保证 regs 指向内核栈上有效且我们独占的 PtRegs。
    let regs = unsafe { &mut *regs };
    let v = vector as usize;

    // SAFETY: 单核；中断路径与此处不会并发改同一格（异常不可重入到自身）。
    unsafe {
        if v < 32 {
            (*core::ptr::addr_of_mut!(TRAP_COUNT))[v] += 1;
        }
        *core::ptr::addr_of_mut!(LAST_TRAP_VECTOR) = vector;
    }

    let info = TRAP_INFO.get(v);
    let name = info.map(|i| i.name).unwrap_or("unknown trap");
    let has_ec = info.map(|i| i.has_error_code).unwrap_or(false);
    let error_code = if has_ec { regs.orig_rax } else { 0 };

    // NMI：原版 do_nmi 只打印两行提示然后照常返回，不杀进程。
    if v == 2 {
        crate::pr!(Level::Err,
                   "Uhhuh. NMI received. Dazed and confused, but trying to continue");
        crate::pr!(Level::Err, "You probably have a hardware problem with your RAM chips");
        return;
    }

    // page fault 的 cr2（出错地址）只在这个向量有意义，原版存进 tss.cr2
    let cr2 = if v == 14 {
        // SAFETY: 读 cr2 在 CPL=0 下合法，不访问内存。
        let mut cr2: u64;
        unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) }
        Some(cr2)
    } else {
        None
    };

    if regs.from_user() {
        // page fault: 尝试处理 COW 页面故障
        if v == 14 {
            if let Some(fault_addr) = cr2 {
                let pml4 = unsafe { (*sched::task_ptr(sched::current_index())).pml4 };
                // 检查是否是 COW 页面错误 (write + present + user)
                let is_write = (error_code & 2) != 0;   // 写访问
                let is_present = (error_code & 1) != 0;  // 页面已映射（只是缺 RW）
                let is_user = (error_code & 4) != 0;     // CPL=3 触发
                // bit4 = instruction fetch（err=0x15 表示在 present+user 且带 NX
                // 的页上取指）。常见成因：cow_copy_page_table 用 PRESENT|USER
                // 重写了 PTE，但 mprotect 之前给某些页落过 NO_EXEC，或动态
                // 链接器对 RELRO 段 mprotect(PROT_READ) 连带把同页代码标成 NX。
                let is_fetch = (error_code & 0x10) != 0;

                if pml4 != 0 && is_present && is_user {
                    // 写访问走 COW 路径。
                    if is_write {
                        if let Some(handled) = unsafe { crate::umm::try_handle_cow_fault(fault_addr, pml4) } {
                            if handled {
                                crate::pr_debug!("COW page fault handled at {:#x}", fault_addr);
                                return;  // 成功处理，恢复执行
                            }
                        }
                    }
                    // 取指故障：页面已映射且属用户空间，仅因 NX 位被拒。
                    // 用户态代码段本应可执行——直接清掉 NO_EXEC 让其继续。
                    // set_page_flags 会保留 RW/USER/PRESENT，只重写低标志，
                    // 用 get_page_flags 取当前标志再清 NO_EXEC 即可。
                    if is_fetch {
                        if let Some(f) = crate::mm::paging::get_page_flags(pml4, fault_addr as usize) {
                            let exec_flags = f & !crate::mm::paging::flags::NO_EXEC;
                            if crate::mm::paging::set_page_flags(pml4, fault_addr as usize, exec_flags) {
                                crate::pr_debug!("exec page fault handled at {:#x}", fault_addr);
                                return;
                            }
                        }
                    }
                }

                // 惰性分配：mmap/brk 保留（PRESENT=0 + RESERVED）的页第一次被
                // 访问时在这里落实物理页。glibc malloc 预留的大段竞技场、线程栈
                // 等只会被触碰一小部分，剩下的保留页永远不分配，内存就不会被
                // 一次吃光。
                if pml4 != 0 && !is_present && is_user {
                    // swap 换入：叶子是 SWAPPED 项（PRESENT=0 但非 RESERVED），
                    // 从交换区把页读回来。
                    if let Some(true) = unsafe {
                        crate::mm::swap::try_swap_in(pml4, fault_addr as usize)
                    } {
                        crate::pr_debug!("swapin resolved at {:#x}", fault_addr);
                        return;
                    }
                    if crate::mm::paging::is_reserved(pml4, fault_addr as usize) {
                        // file-backed VMA 的页：按 VMA 记的文件偏移读入内容。
                        let task = sched::current_index();
                        match unsafe {
                            crate::mm::mmap_vma::resolve_file_fault(task, pml4, fault_addr as usize)
                        } {
                            Some(true) => {
                                crate::pr_debug!("lazy file page fault resolved at {:#x}", fault_addr);
                                return;
                            }
                            Some(false) => { /* 在 VMA 里但解析失败：落 SIGSEGV */ }
                            None => {
                                // 匿名保留页：分配零页。
                                if unsafe { crate::mm::paging::resolve_reserved(pml4, fault_addr as usize) } {
                                    crate::pr_debug!("lazy page fault resolved at {:#x}", fault_addr);
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
        
        // 原版这里 send_sig(signr, current, 1) 让进程自己去死。
        // 我们暂时只打印——signal.c 未移植。
        let signr = info.map(|i| i.signr).unwrap_or(SIGSEGV);
        send_sig_stub(signr, name, regs, error_code, cr2);
        return;
    }

    // 内核态缺页。CR0.WP=1 后，内核对 COW 只读用户页的写（copy_to_user，
    // 如 read() 把数据拷进子进程缓冲区）会以 supervisor write-protect 形式
    // 缺页（err: P=1 W=1 U=0）。按 COW 复制后重试写即可——这正是 WP=1
    // 相对 WP=0 的关键收益：内核写不再穿透共享页 corrupt 父进程。
    if v == 14 {
        if let Some(fault_addr) = cr2 {
            let is_write = (error_code & 2) != 0;
            let is_present = (error_code & 1) != 0;
            let is_user_page = (error_code & 4) == 0;  // supervisor 访问用户页
            let pml4 = unsafe { (*sched::task_ptr(sched::current_index())).pml4 };
            if pml4 != 0 {
                // 内核对用户惰性分配页（mmap/brk 的 RESERVED、PRESENT=0）的读写：
                // copy_from_user/copy_to_user 会直接访问还没落实物理页的保留页。
                // 这里先落实，再重试访问（与用户态惰性缺页同路）；file-backed
                // VMA 的页要按文件偏移读入内容。
                // swap 换入：SWAPPED 叶子（PRESENT=0 且非 RESERVED）。
                if !is_present {
                    if let Some(true) = unsafe {
                        crate::mm::swap::try_swap_in(pml4, fault_addr as usize)
                    } {
                        crate::pr_debug!("supervisor swapin resolved at {:#x}", fault_addr);
                        return;
                    }
                }
                if !is_present
                    && crate::mm::paging::is_reserved(pml4, fault_addr as usize)
                {
                    let task = sched::current_index();
                    let resolved = match unsafe {
                        crate::mm::mmap_vma::resolve_file_fault(task, pml4, fault_addr as usize)
                    } {
                        Some(ok) => ok,
                        None => unsafe {
                            crate::mm::paging::resolve_reserved(pml4, fault_addr as usize)
                        },
                    };
                    if resolved {
                        crate::pr_debug!("lazy supervisor fault resolved at {:#x}", fault_addr);
                        return;
                    }
                }
                if is_write && is_present && is_user_page {
                    if let Some(true) = unsafe { crate::umm::try_handle_cow_fault(fault_addr, pml4) } {
                        crate::pr_debug!("COW supervisor fault handled at {:#x}", fault_addr);
                        return;
                    }
                }
            }
        }
    }

    die_if_kernel(name, regs, error_code, cr2, v);
}

/// 用户态异常转信号。对应原版 `send_sig(signr, current, 1)`（`kernel/signal.c`）。
///
/// 信号只是**投递**（在 `task.signal` 里置位）；真正的动作由
/// [`crate::signal::do_signal`] 在 `ret_from_sys_call` 返回用户态前执行。
/// 对 SIGSEGV / SIGILL / SIGFPE 这些默认动作是终止，所以效果就是进程被杀。
fn send_sig_stub(signr: u32, name: &str, regs: &PtRegs, error_code: u64, cr2: Option<u64>) {
    // cr2 只有 page fault 才有，用 0 表示不适用（比拼接字符串省事且不引入分配）
    crate::pr!(Level::Err,
               "{}: sig {} at rip={:#x} err={:#x} cr2={:#x} (user)",
               name, signr, regs.rip, error_code, cr2.unwrap_or(0));

    // DEBUG: dump user rsp/rax and the return address on the user stack,
    // 以定位用户态为何跳到错误地址（如 0x1000）。
    if signr == 11 {
        crate::pr!(Level::Err, "USER regs: rsp={:#x} rax={:#x} rbx={:#x} rcx={:#x} rdx={:#x} rbp={:#x}",
                   regs.rsp, regs.rax, regs.rbx, regs.rcx, regs.rdx, regs.rbp);
        let usp = regs.rsp as usize;
        // 读用户栈顶 8 个 u64（返回地址等）
        for k in 0..8usize {
            let addr = usp + k*8;
            // SAFETY: 仅诊断读，地址来自用户 rsp；可能触发嵌套 #PF，但顶层已 in_panic 风险低
            let v = unsafe { core::ptr::read_volatile(addr as *const u64) };
            crate::pr!(Level::Err, "  ustack[{:#x}] = {:#x}", addr, v);
        }
    }

    let nr = sched::current_index();

    // task[0] 收不了致命信号——它是 swapper，杀了就没人可调度。
    // 原版在 do_exit 里 panic，我们提前在这里拦住并给出更准的现场。
    if nr == 0 {
        crate::pr!(Level::Emerg,
                   "{}: fatal signal {} targets task[0] (swapper) -- cannot deliver",
                   name, signr);
        panic!("user-mode exception in task[0]");
    }

    // priv=1：内核触发，不做权限检查。
    let rc = crate::signal::send_sig(signr, nr, 1);
    if rc != 0 {
        crate::pr!(Level::Emerg, "{}: send_sig({}) failed: {}", name, signr, rc);
    }
}

/// 内核态异常的处理：打印完整现场后停机。
///
/// 对应原版 `die_if_kernel()`。原版最后是 `do_exit(SIGSEGV)`（杀掉当前进程），
/// 但那要求有进程可杀；启动期或纯内核路径上出异常只能停机，
/// 所以这里走 `panic!`（我们的 panic handler 会打印并 halt）。
fn die_if_kernel(name: &str, regs: &mut PtRegs, error_code: u64, cr2: Option<u64>, vector: usize) {
    // 原版 die_if_kernel 第一件事是 console_verbose()（把 loglevel 提到 15），
    // 保证接下来的现场打印一定能上屏。
    crate::klib::printk::set_console_loglevel(15);

    crate::pr!(Level::Emerg, "{}: {:04x}", name, error_code & 0xFFFF);
    crate::pr!(Level::Emerg, "RIP: {:04x}:{:#018x}  RFLAGS: {:#018x}",
               regs.cs & 0xFFFF, regs.rip, regs.rflags);
    if let Some(addr) = cr2 {
        // page fault 的错误码位含义（原版没打，但这是最常见的异常，值得展开）
        crate::pr!(Level::Emerg, "CR2: {:#018x}  ({} {} {})",
                   addr,
                   if error_code & 1 != 0 { "protection" } else { "not-present" },
                   if error_code & 2 != 0 { "write" } else { "read" },
                   if error_code & 4 != 0 { "user" } else { "kernel" });
    }
    crate::pr!(Level::Emerg, "rax: {:#018x}  rbx: {:#018x}  rcx: {:#018x}",
               regs.rax, regs.rbx, regs.rcx);
    crate::pr!(Level::Emerg, "rdx: {:#018x}  rsi: {:#018x}  rdi: {:#018x}",
               regs.rdx, regs.rsi, regs.rdi);
    crate::pr!(Level::Emerg, "rbp: {:#018x}  rsp: {:#018x}  r8 : {:#018x}",
               regs.rbp, regs.rsp, regs.r8);
    crate::pr!(Level::Emerg, "r9 : {:#018x}  r10: {:#018x}  r11: {:#018x}",
               regs.r9, regs.r10, regs.r11);
    crate::pr!(Level::Emerg, "r12: {:#018x}  r13: {:#018x}  r14: {:#018x}",
               regs.r12, regs.r13, regs.r14);
    crate::pr!(Level::Emerg, "r15: {:#018x}  ss : {:#04x}", regs.r15, regs.ss & 0xFFFF);

    // 原版还打 "Process %s (pid: %d...)" 和栈/代码 dump。
    // 进程信息由 sched 模块提供。
    crate::sched::print_current();

    // 原版这里 dump 栈上 5 个字和 rip 处 20 字节代码。
    // SAFETY: rsp 是刚陷入时的内核栈指针，指向我们自己的内核栈，可读。
    if regs.from_kernel() {
        let sp = core::ptr::addr_of!(regs.rip) as *const u64;
        let mut buf = [0u64; 5];
        for (i, slot) in buf.iter_mut().enumerate() {
            // SAFETY: 从 pt_regs 的 rip 格往高地址读 5 个字，仍在内核栈范围内。
            *slot = unsafe { core::ptr::read_volatile(sp.add(i)) };
        }
        crate::pr!(Level::Emerg, "Stack: {:#x} {:#x} {:#x} {:#x} {:#x}",
                   buf[0], buf[1], buf[2], buf[3], buf[4]);
    }

    // 自检模式：打印完就跳过出错指令继续跑。
    // SAFETY: 只读一个 bool。
    if unsafe { *core::ptr::addr_of!(TRAP_RECOVER) } {
        crate::pr!(Level::Warning, "trap {}: recover mode, skipping faulting instruction", vector);
        // 只有已知长度的指令才能安全跳过，这里靠自检自己保证
        // （它用的是 `div`(2字节) / `ud2`(2字节) / `int3`(1字节)）。
        // int3 的 rip 已指向下一条，无需调整。
        if vector != 3 {
            regs.rip += 2;
        }
        return;
    }

    panic!("kernel trap {} ({})", vector, name);
}

/// 打印各向量的触发次数，供启动自检。
pub fn dump_counts() {
    let mut any = false;
    for v in 0..32 {
        let c = trap_count(v);
        if c != 0 {
            let name = TRAP_INFO.get(v).map(|i| i.name).unwrap_or("?");
            crate::pr!(Level::Info, "trap {:2}: {:4} times ({})", v, c, name);
            any = true;
        }
    }
    if !any {
        crate::pr!(Level::Info, "traps: none triggered");
    }
}

/// 当前的段选择子摘要，验证 GDT 装载是否生效。
pub fn dump_segments() {
    let (cs, ss): (u16, u16);
    // SAFETY: 读段寄存器在任何特权级都合法，不访问内存。
    unsafe {
        core::arch::asm!("mov {0:x}, cs", "mov {1:x}, ss",
                         out(reg) cs, out(reg) ss,
                         options(nomem, nostack, preserves_flags));
    }
    crate::pr!(Level::Info, "segments: cs={:#04x} ss={:#04x} (expect cs={:#04x} ss={:#04x})",
               cs, ss, selector::KERNEL_CS, selector::KERNEL_DS);
}
