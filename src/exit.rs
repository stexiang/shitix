//! 进程退出处理。参考 linux-1.0.9 的 `kernel/exit.c`。
//!
//! ## 功能
//!
//! - 进程终止（do_exit）
//! - 任务资源释放（release）
//! - 父进程通知
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `exit.c` | 进程退出核心实现 |

use crate::sched::{self, task::TaskState};
use crate::signal::{self, Signal};

/// 退出码类型
pub type ExitCode = i32;

/// 进程终止。参考 `kernel/exit.c:do_exit()`。
///
/// 执行以下操作：
/// 1. 释放内存页表
/// 2. 关闭所有打开的文件
/// 3. 通知父进程
/// 4. 将子进程托付给 init
/// 5. 设置进程状态为 ZOMBIE
/// 6. 调度到新进程
///
/// # Arguments
///
/// * `code` - 退出码，会存入 `exit_code` 字段
pub fn do_exit(code: ExitCode) -> ! {
    let nr = sched::current_index();

    // task[0] 是 swapper，退出它就没人可调度了。原版
    // `if (current == task[0]) panic("task[0] exiting")`。
    if nr == 0 {
        panic!("task[0] (swapper) trying to exit");
    }

    // SAFETY: 只读当前任务。
    let pid = unsafe { (*sched::task_ptr(nr)).pid };
    crate::sprintln!("[INFO] process {} exiting with code {}", pid, code);

    // SAFETY: 正在终止当前进程；单核不抢占，这段独占任务表。
    unsafe {
        // VFORK：子进程退出，唤醒被挂起的父进程（若子进程没走到 execve）。
        let vp = (*sched::task_ptr(nr)).vfork_parent;
        if vp != 0 {
            (*sched::task_ptr(nr)).vfork_parent = 0;
            let fl = crate::irq::local_irq_save();
            crate::sched::sched_lock();
            (*sched::task_ptr(vp)).state = TaskState::Running;
            crate::sched::sched_unlock();
            crate::irq::restore_flags(fl);
        }
        // 关闭所有打开的文件。对应原版 do_exit 里那个
        // `for (i=0 ; i<NR_OPEN ; i++) if (current->filp[i]) sys_close(i)`。
        // FD 表目前还是全局的（见 fs::open），所以这里只在最后一个用户
        // 进程退出时才真正有意义——等 filp[] 进 Task 后语义就对了。
        crate::fs::open::close_all();

        // 清掉 file-backed mmap 的 VMA 记账（地址空间随进程消亡）。
        crate::mm::mmap_vma::clear(nr);

        // 归还该进程还留在交换区里的槽位（原版 free_page_tables 里的
        // swap_free(entry)）。
        crate::mm::swap::free_task_swap((*sched::task_ptr(nr)).pml4);

        // 摘掉该任务的全部 SysV 共享内存附加（原版 shm_exit）。
        crate::mm::shm::exit_task(nr, (*sched::task_ptr(nr)).pml4);

        // 把子进程托付出去。原版遍历 p_cptr 链把孩子挂到 init 名下，
        // 我们的亲子关系是 `parent` 下标，所以扫一遍任务表。
        reparent_children(nr);

        let task = sched::task_ptr(nr);
        (*task).exit_code = code;
        let fl = crate::irq::local_irq_save();
        crate::sched::sched_lock();
        (*task).state = TaskState::Zombie;
        (*task).on_cpu = -1;
        crate::sched::sched_unlock();
        crate::irq::restore_flags(fl);

        // 通知父进程：SIGCHLD + 唤醒它的 wait 睡眠。
        // 必须在 state = Zombie 之后，否则父进程醒来时还看不到僵尸。
        notify_parent(nr);
    }

    // 调度到新进程，当前进程不会再被选中（状态是 Zombie）
    // SAFETY: do_exit 是进程的最终点，调用 schedule 是安全的
    unsafe { sched::schedule() };

    // 这行永远不会被执行，因为 schedule() 不会返回
    loop {
        crate::sprintln!("[EMERG] BUG: do_exit returned!");
        // SAFETY: 同上
        unsafe { sched::schedule() };
    }
}

/// 把退出进程的孩子改挂到 init（task[1]）名下，没有 init 就挂 swapper。
/// 对应原版 `do_exit` 里那段 `p->p_pptr = task[1]` 的循环。
///
/// # Safety
/// 调用者必须独占任务表（关中断或单核不抢占）。
unsafe fn reparent_children(dying: usize) {
    // SAFETY: 契约转交；下标都在 0..NR_TASKS 内。
    unsafe {
        // init 存在就用 init，否则退回 swapper。
        let foster = if (*sched::task_ptr(1)).state != TaskState::Unused { 1 } else { 0 };

        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state == TaskState::Unused || (*t).parent != dying {
                continue;
            }
            (*t).parent = foster;
            // 孩子已经是僵尸的话，新养父得收到 SIGCHLD，否则永远没人收尸。
            if (*t).state == TaskState::Zombie {
                let _ = signal::send_sig(Signal::SIGCHLD as u32, foster, 1);
                wake_up_waiter(foster);
            }
        }
    }
}

/// 唤醒卡在 [`sys_wait4`] 里可打断睡眠的任务。
///
/// # Safety
/// 调用者必须独占任务表。
unsafe fn wake_up_waiter(task_idx: usize) {
    // SAFETY: 契约转交。
    unsafe {
        let t = sched::task_ptr(task_idx);
        if (*t).state == TaskState::Interruptible {
            let fl = crate::irq::local_irq_save();
            crate::sched::sched_lock();
            (*t).state = TaskState::Running;
            crate::sched::sched_unlock();
            crate::irq::restore_flags(fl);
        }
    }
}

/// 通知父进程有子进程退出。参考 `kernel/exit.c:notify_parent()`。
///
/// 向父进程发送 SIGCHLD 信号，并唤醒父进程的等待队列。
fn notify_parent(child_idx: usize) {
    // SAFETY: 修改子进程和父进程的信号
    unsafe {
        let child = sched::task_ptr(child_idx);
        let parent_idx = (*child).parent;
        
        // 发送 SIGCHLD 给父进程
        // 使用 priv=1 因为这是内核触发的
        let _ = signal::send_sig(Signal::SIGCHLD as u32, parent_idx, 1);

        // 唤醒卡在 wait4 里的父进程。原版是
        // `wake_up_interruptible(&parent->wait_chldexit)`；我们没有 per-task
        // 等待队列（Task 里没有 wait_chldexit 字段），直接把可打断睡眠的
        // 父进程放回运行态——语义一致，因为 wait4 醒来后会重新扫僵尸。
        wake_up_waiter(parent_idx);
    }
}

/// 释放任务资源。参考 `kernel/exit.c:release()`。
///
/// 这是进程最终被回收时调用的，清理所有残留资源。
///
/// # Arguments
///
/// * `task_idx` - 要释放的任务索引
///
/// # Returns
///
/// * 0 - 成功
/// * -1 - 任务不存在或正在释放自己
pub fn release(task_idx: usize) -> i32 {
    // SAFETY: 只在确定任务已退出时调用
    unsafe {
        let task = sched::task_ptr(task_idx);
        
        // 检查是否是当前任务（不允许释放自己）
        if task_idx == sched::current_index() {
            crate::sprintln!("[EMERG] release: task releasing itself!");
            return -1;
        }
        
        // 检查任务是否存在（状态不是 Unused）
        if (*task).state == TaskState::Unused {
            crate::sprintln!("[EMERG] release: task {} not found!", task_idx);
            return -1;
        }
        
        // 检查内核栈完整性。栈底魔数被踩说明这个任务溢出过栈，
        // 对应原版 die_if_kernel 里那句 STACK_MAGIC 检查。
        if !(*task).stack_ok() {
            crate::sprintln!(
                "[EMERG] release: task {} corrupted its kernel stack (magic gone)",
                task_idx
            );
        }

        // 从调度环中移除
        // SAFETY: 移除任务出调度环
        remove_from_runqueue(task_idx);

        // 释放内核栈页。原版 release() 里 `free_page(p->kernel_stack_page)`。
        // init_task 的栈是 head.S 的静态栈（kernel_stack == 0），不能还给分配器。
        let stack = (*task).kernel_stack;
        if stack != 0 {
            crate::sched::free_kstack(stack as usize);
            (*task).kernel_stack = 0;
        }

        // 释放用户地址空间的全部用户页 + PDPT/PD/PT 子树，再回收 PML4 页。
        // （在父进程上下文回收，此时 CR3 已切走，安全。）之前只 free_page(pml4)
        // 回收 PML4 页自身，用户物理页与页表页全部泄漏，gcc/g++ 连跑几次就把
        // 空闲页耗尽。
        let pml4 = (*task).pml4;
        if pml4 != 0 {
            unsafe { crate::mm::paging::free_user_pages(pml4) };
            crate::mm::free_page(pml4);
            (*task).pml4 = 0;
            (*task).tss.cr3 = 0;
        }

        // 信号处理表复位，否则槽位复用时新进程会继承旧 handler。
        signal::reset_sigactions(task_idx);

        // 释放 per-task pwd/root inode 引用
        let pwd = (*task).pwd;
        let root = (*task).root;
        if pwd != crate::fs::inode::NIL && pwd != root {
            crate::fs::inode::iput(pwd);
        }
        if root != crate::fs::inode::NIL {
            crate::fs::inode::iput(root);
        }
        (*task).pwd = crate::fs::inode::NIL;
        (*task).root = crate::fs::inode::NIL;

        // 重置任务状态
        let fl = crate::irq::local_irq_save();
        crate::sched::sched_lock();
        (*task).state = TaskState::Unused;
        (*task).on_cpu = -1;
        crate::sched::sched_unlock();
        crate::irq::restore_flags(fl);

        crate::sprintln!("[INFO] released task slot {}", task_idx);
        0
    }
}

// =============================================================================
// wait4
// =============================================================================

/// `wait4` 的 options 位。对应原版 `include/linux/wait.h`。
pub mod wait_flags {
    /// 没有僵尸子进程时立刻返回而不是睡下
    pub const WNOHANG: u64 = 1;
    /// 也报告被 SIGSTOP 停住的子进程
    pub const WUNTRACED: u64 = 2;
}

/// `sys_wait4` 的核心。参考 `kernel/exit.c:sys_wait4()`。
///
/// - `pid > 0`：等这个具体的 pid
/// - `pid == -1`：等任意子进程
/// - `pid == 0`：等同进程组的子进程
/// - `pid < -1`：等进程组 `-pid` 的子进程
///
/// 找到僵尸就取走 exit_code、[`release`] 掉槽位、返回它的 pid。
/// 没有僵尸但有活着的子进程：`WNOHANG` 时返回 0，否则可打断地睡下。
///
/// # Arguments
///
/// * `pid` - 上面四种语义
/// * `stat_addr` - 非 0 时把状态字写到这个地址（用户指针，目前是恒等映射）
/// * `options` - 见 [`wait_flags`]
///
/// # Returns
///
/// 收到尸的子进程 pid；`WNOHANG` 且无僵尸时 0；没有子进程时 `-ECHILD`；
/// 被信号打断时 `-EINTR`。
///
/// # Safety
/// 只能在系统调用上下文调用（有当前任务、可以睡眠）。`stat_addr` 若非 0
/// 必须是当前地址空间里可写的 4 字节。
pub unsafe fn sys_wait4(pid: i64, stat_addr: u64, options: u64) -> i64 {
    use crate::klib::errno::{ECHILD, EINTR};

    let me = sched::current_index();

    loop {
        let mut had_child = false;

        // SAFETY: 单核不抢占，这段独占任务表；下标都在界内。
        let found = unsafe {
            let my_pgrp = (*sched::task_ptr(me)).pgrp;
            let mut found = None;

            for i in 0..sched::NR_TASKS {
                if i == me {
                    continue;
                }
                let t = sched::task_ptr(i);
                if (*t).state == TaskState::Unused || (*t).parent != me {
                    continue;
                }

                // pid 过滤，四种语义见函数文档。
                let want = match pid {
                    p if p > 0 => (*t).pid as i64 == p,
                    0 => (*t).pgrp == my_pgrp,
                    -1 => true,
                    p => (*t).pgrp as i64 == -p,
                };
                if !want {
                    continue;
                }
                had_child = true;

                match (*t).state {
                    TaskState::Zombie => {
                        found = Some((i, (*t).pid, (*t).exit_code));
                        break;
                    }
                    // WUNTRACED：报告停住的子进程但不收尸。
                    TaskState::Stopped if options & wait_flags::WUNTRACED != 0 => {
                        let code = (*t).exit_code;
                        if code != 0 {
                            (*t).exit_code = 0;
                            // 原版状态字：低 8 位 0x7f 表示 stopped，高 8 位是信号号。
                            found = Some((sched::NR_TASKS, (*t).pid, (code << 8) | 0x7f));
                            break;
                        }
                    }
                    _ => {}
                }
            }
            found
        };

        if let Some((slot, child_pid, code)) = found {
            // `exit_code` 现在存的就是最终状态字（sys_exit 已把退出码编成
            // `(code & 0xff) << 8`，信号终止存原始信号号 1..=31），wait4
            // 直接透传，不再需要区分「1..=31 是退出码还是信号」。
            let status = code;
            if stat_addr != 0 {
                // SAFETY: 契约保证 stat_addr 可写 4 字节；页表恒等映射。
                unsafe { core::ptr::write_volatile(stat_addr as *mut i32, status) };
            }
            if slot != sched::NR_TASKS {
                // 父进程把子进程的时间累加过来。原版
                // `current->cutime += p->utime + p->cutime`（我们没有 cutime）。
                // SAFETY: slot 是刚找到的僵尸槽位。
                unsafe {
                    let (cu, cs) = {
                        let c = sched::task_ptr(slot);
                        ((*c).utime, (*c).stime)
                    };
                    let p = sched::task_ptr(me);
                    (*p).utime += cu;
                    (*p).stime += cs;
                }
                release(slot);
            }
            return child_pid as i64;
        }

        if !had_child {
            return -(ECHILD as i64);
        }
        if options & wait_flags::WNOHANG != 0 {
            return 0;
        }

        // 有活着的子进程但都还没退出：可打断地睡下，等 notify_parent 叫醒。
        // SAFETY: 系统调用上下文，me != 0（task[0] 不会走到这里，见 do_exit 的检查）。
        unsafe {
            let t = sched::task_ptr(me);
            let fl = crate::irq::local_irq_save();
            crate::sched::sched_lock();
            (*t).state = TaskState::Interruptible;
            crate::sched::sched_unlock();
            crate::irq::restore_flags(fl);
            sched::schedule();

            // 醒来先看是不是被信号打断的。原版返回 -ERESTARTSYS，
            // 我们没有系统调用重启机制，返回 -EINTR。
            if (*t).has_pending_signal() {
                return -(EINTR as i64);
            }
        }
    }
}

/// 从运行队列中移除任务。参考 `kernel/exit.c:REMOVE_LINKS()`。
fn remove_from_runqueue(task_idx: usize) {
    // SAFETY: 修改调度环
    unsafe {
        let task = sched::task_ptr(task_idx);
        let next = (*task).next;
        let prev = (*task).prev;
        
        let next_task = sched::task_ptr(next);
        let prev_task = sched::task_ptr(prev);
        
        (*next_task).prev = prev;
        (*prev_task).next = next;
    }
}

/// 检查任务指针是否有效。参考 `kernel/exit.c:bad_task_ptr()`。
///
/// # Returns
///
/// * true - 任务指针无效
/// * false - 任务指针有效
pub fn bad_task_ptr(task_idx: usize) -> bool {
    if task_idx >= sched::NR_TASKS {
        return true;
    }
    // SAFETY: 只读
    unsafe {
        let task = sched::task_ptr(task_idx);
        (*task).state == TaskState::Unused
    }
}

/// 获取退出状态信息。用于 sys_wait4 等系统调用。
#[derive(Debug)]
pub struct ExitStatus {
    /// 退出码
    pub exit_code: i32,
    /// 退出信号（如果有）
    pub signal: Option<Signal>,
}

impl ExitStatus {
    /// 从 exit_code 字段解析退出状态
    pub fn from_exit_code(code: i32) -> Self {
        // Linux 用高 8 位存信号，低 8 位存退出码
        let signal_num = (code >> 8) as u32;
        let exit_code = code & 0xFF;
        
        let signal = if signal_num > 0 {
            Signal::from_u32(signal_num)
        } else {
            None
        };
        
        ExitStatus {
            exit_code,
            signal,
        }
    }
    
    /// 正常退出（无信号）
    pub fn normal(code: i32) -> Self {
        ExitStatus {
            exit_code: code,
            signal: None,
        }
    }
    
    /// 信号退出
    pub fn signaled(sig: Signal) -> Self {
        ExitStatus {
            exit_code: 0,
            signal: Some(sig),
        }
    }
    
    /// 是否正常退出
    pub fn is_normal(&self) -> bool {
        self.signal.is_none()
    }
    
    /// 是否被信号终止
    pub fn is_signaled(&self) -> bool {
        self.signal.is_some()
    }
}

/// 初始化退出处理模块
pub fn init() {
    crate::sprintln!("exit: process exit handling initialized");
}

// =============================================================================
// Self-Tests
// =============================================================================

/// 运行退出处理自检
pub fn selftest() {
    crate::sprintln!("--- exit selftest ---");

    // 测试退出状态解析
    let status1 = ExitStatus::from_exit_code(0);
    assert!(status1.is_normal());
    assert!(status1.exit_code == 0);

    let status2 = ExitStatus::from_exit_code(0x0900); // SIGKILL = 9
    assert!(status2.is_signaled());
    assert!(status2.signal == Some(Signal::SIGKILL));

    let status3 = ExitStatus::normal(42);
    assert!(status3.is_normal());
    assert!(status3.exit_code == 42);

    crate::sprintln!("exit selftest: exit status parsing -> ok");
}

// =============================================================================
// fork / exit / wait4 端到端自检
// =============================================================================

/// 子进程退出时用的退出码，父进程靠它确认收到的是自己那个孩子。
const FORK_TEST_EXIT_CODE: i32 = 42;

/// fork 出来的测试子进程的进程体。
///
/// 由 [`fork_selftest`] 把子进程 pt_regs 的 `rip` 指到这里，所以子进程
/// 第一次被调度时走 `ret_from_fork` → `ret_from_sys_call` → `iret`，
/// 直接 iret 到这个函数（而不是 iret 回父进程 `int 0x80` 的下一条指令
/// ——那条路会让父子共用父进程的内核栈，见 [`fork_selftest`] 的说明）。
///
/// 这是 execve 迟早要干的同一件事：换掉子进程的返回现场。
extern "C" fn fork_test_child() -> ! {
    // 先自己给自己发一个 SIGTERM 再走 do_signal，验证「默认动作 = 终止」
    // 这条路。注意：**不能**指望 entry.S 的 do_signal 钩子在这里生效——
    // ret_from_sys_call 对内核态返回（PT_CS == KERNEL_CS）是故意跳过信号
    // 投递的（原版同样如此），所以这里显式调一次，测的是决策逻辑本身。
    // SAFETY: 进程上下文，当前任务就是这个子进程。
    unsafe {
        let me = sched::current_index();
        let _ = signal::send_sig(Signal::SIGTERM as u32, me, 1);
        // do_signal 会走到 do_exit(SIGTERM)，不返回。
        signal::do_signal(core::ptr::null_mut());
    }
    // do_signal 没能终止我们的话，退一个可辨认的码兜底，别悄悄跑下去。
    do_exit(FORK_TEST_EXIT_CODE)
}

/// fork + exit + wait4 的端到端自检。
///
/// 覆盖：`sys_fork` 真造出子进程（父子拿到不同 pid、子进程 pt_regs 里
/// `rax == 0`、子进程进了调度环）、子进程被调度后走 `ret_from_fork` 真的
/// 跑起来、`do_signal` 对默认动作信号执行终止、`do_exit` 转 ZOMBIE 并通知
/// 父进程、父进程 `wait4` 收尸并释放槽位与内核栈。
///
/// **必须在内核线程里跑**（不能在 task[0]）：wait4 会睡。
///
/// # Safety
/// 需要 IDT 就绪、调度器可用、当前不是 task[0]。
pub unsafe fn fork_selftest() {
    use crate::sched::task::{KERNEL_STACK_SIZE, TaskState};
    use crate::syscall::{self, nr};

    crate::sprintln!("--- fork/exit/wait4 selftest ---");

    let mut ok = true;
    let mut check = |cond: bool, what: &str, got: i64| {
        if !cond {
            ok = false;
            crate::sprintln!("fork: {} FAILED (got {})", what, got);
        }
    };

    // SAFETY: 契约转交。
    let my_pid = unsafe { (*sched::task_ptr(sched::current_index())).pid };
    let free_before = crate::mm::nr_free_pages();

    // ---- fork ----
    // 走真的 int 0x80：这样 pt_regs 是 SAVE_ALL 压出来的完整现场，
    // sys_fork 复制的就是它。
    // SAFETY: IDT 就绪，我们在内核线程栈上，pt_regs 放得下。
    let child_pid = unsafe { syscall::syscall0(nr::FORK) };
    check(child_pid > 0, "fork returned a pid", child_pid);
    check(child_pid != my_pid as i64, "child pid differs from parent", child_pid);
    if child_pid <= 0 {
        crate::sprintln!("fork: -> FAIL (cannot continue)");
        return;
    }

    // ---- 找到子进程槽位，检查 fork 布置的现场 ----
    // SAFETY: 单核不抢占；child_pid 刚由 fork 分配，槽位一定还在。
    let child_slot = unsafe {
        let mut found = sched::NR_TASKS;
        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state != TaskState::Unused && (*t).pid as i64 == child_pid {
                found = i;
                break;
            }
        }
        found
    };
    check(child_slot < sched::NR_TASKS, "child slot found", child_slot as i64);
    if child_slot >= sched::NR_TASKS {
        crate::sprintln!("fork: -> FAIL (cannot continue)");
        return;
    }

    // SAFETY: child_slot 有效；子进程还没被调度过（我们没让出 CPU），
    // 它的内核栈上就是 fork 刚写的那份现场，可以安全读改。
    unsafe {
        let child = sched::task_ptr(child_slot);
        check((*child).parent == sched::current_index(), "child's parent is us",
              (*child).parent as i64);
        check((*child).state == TaskState::Running, "child is runnable", 0);
        check((*child).kernel_stack != 0, "child got a kernel stack", 0);
        check((*child).stack_ok(), "child stack magic present", 0);

        // 子进程在调度环里吗？从自己出发绕一圈找它。
        let mut in_ring = false;
        let mut p = (*sched::task_ptr(sched::current_index())).next;
        for _ in 0..sched::NR_TASKS {
            if p == child_slot {
                in_ring = true;
                break;
            }
            p = (*sched::task_ptr(p)).next;
        }
        check(in_ring, "child linked into the scheduling ring", 0);

        // pt_regs 就在内核栈顶往下 0xa8 字节处（见 sys_fork 的布局注释）。
        let stack_top = (*child).kernel_stack + KERNEL_STACK_SIZE as u64;
        let regs = (stack_top - core::mem::size_of::<crate::traps::PtRegs>() as u64)
            as *mut crate::traps::PtRegs;
        check((*regs).rax == 0, "child's saved rax is 0 (fork returns 0)",
              (*regs).rax as i64);
        check((*regs).rip == (*sched::task_ptr(sched::current_index())).tss.trap_no
              || (*regs).rip != 0, "child's saved rip is non-zero", (*regs).rip as i64);

        // 换掉子进程的返回现场，让它 iret 到 fork_test_child。
        //
        // 为什么必须换：内核态 fork 出来的子进程，pt_regs 里的 rsp 指向
        // **父进程**的内核栈（int 0x80 时的 rsp）。照原样 iret 会让父子
        // 在同一个栈上跑同一段代码，必然互踩。用户态 fork 没这问题（各有
        // 自己的用户栈），所以这一步是「内核态 fork」特有的，正是 execve
        // 之后要做的事：给子进程一份自己的返回现场。
        (*regs).rip = fork_test_child as *const () as u64;
        // 子进程自己的内核栈，留出栈顶 0x100 字节（那里是刚被 iret 弹掉的
        // switch_to 帧 + pt_regs，逻辑上已经不用了，留白防手误）。
        (*regs).rsp = stack_top - 0x100;
    }

    // ---- wait4 收尸 ----
    let mut status: i32 = -1;
    // SAFETY: IDT 就绪；status 在本内核线程栈上，落在恒等映射低 1GB 内。
    let reaped = unsafe {
        syscall::syscall3(nr::WAIT4, (-1i64) as u64,
                          &raw mut status as u64, 0)
    };
    check(reaped == child_pid, "wait4 reaped our child", reaped);

    // 子进程被 SIGTERM 终止：do_exit 收到的是信号号，encode_status 原样保留。
    let sigterm = Signal::SIGTERM as i32;
    check(status == sigterm, "status reports termination by SIGTERM", status as i64);

    // ---- 槽位与内核栈都回收了吗 ----
    // SAFETY: 收尸后槽位应已复位。
    unsafe {
        let child = sched::task_ptr(child_slot);
        check((*child).state == TaskState::Unused, "child slot released", 0);
        check((*child).kernel_stack == 0, "child kernel stack freed", 0);
    }
    let free_after = crate::mm::nr_free_pages();
    check(free_after == free_before, "no page leak across fork+wait",
          free_after as i64 - free_before as i64);

    // 再 wait 一次应该说「没孩子了」。
    // SAFETY: 同上。
    let again = unsafe { syscall::syscall3(nr::WAIT4, (-1i64) as u64, 0, 0) };
    check(again == -(crate::klib::errno::ECHILD as i64), "second wait4 -> -ECHILD", again);

    if ok {
        crate::sprintln!("fork: fork/exit/wait4 + SIGTERM default action -> ok");
    }
    crate::sprintln!("fork: selftest done");
}
