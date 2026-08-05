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
    crate::sprintln!("[INFO] process {} exiting with code {}", 
        // SAFETY: 只读
        unsafe {
            let task = sched::task_ptr(sched::current_index());
            (*task).pid
        },
        code);

    // SAFETY: 正在终止当前进程，持有调度器控制权
    unsafe {
        let task = sched::task_ptr(sched::current_index());
        
        // 释放内存页表（如果有的话）
        // TODO: 实现 free_page_tables
        
        // 关闭所有打开的文件
        // TODO: 实现文件关闭逻辑
        
        // 通知父进程
        notify_parent(sched::current_index());
        
        // 处理子进程 - 托付给 init (task[1]) 或 swapper (task[0])
        // 这是简化的版本，完整的进程树维护更复杂
        // TODO: 实现完整的子进程收养逻辑
        
        // 设置退出状态
        (*task).state = TaskState::Zombie;
        
        // 注意：Rust 中没有 "never returns" 的语义
        // 我们需要手动调度
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
        
        // 唤醒父进程（如果有等待子进程退出的）
        // TODO: 实现等待队列唤醒
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
        
        // 检查内核栈完整性
        // TODO: 实现栈完整性检查
        
        // 从调度环中移除
        // SAFETY: 移除任务出调度环
        remove_from_runqueue(task_idx);
        
        // 释放内存页表
        // TODO: 实现 free_page_tables
        
        // 重置任务状态
        (*task).state = TaskState::Unused;
        
        crate::sprintln!("[INFO] released task slot {}", task_idx);
        0
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
