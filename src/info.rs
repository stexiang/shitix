//! 系统信息。参考 linux-1.0.9 的 `kernel/info.c`。
//!
//! ## 功能
//!
//! - 系统信息查询（sysinfo）
//! - 内存统计
//! - 进程计数
//! - 系统运行时间
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `info.c` | sysinfo 系统调用实现 |

use crate::sched;

/// 系统信息结构。对应 `struct sysinfo`。
///
/// 包含系统运行时统计信息。
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SysInfo {
    /// 系统运行时间（秒）
    pub uptime: i64,
    /// 1/5/15 分钟平均负载
    pub loads: [u64; 3],
    /// 总物理内存（页面数）
    pub totalram: u64,
    /// 可用物理内存（页面数）
    pub freeram: u64,
    /// 共享内存（页面数）
    pub sharedram: u64,
    /// 用于缓冲区的内存（页面数）
    pub bufferram: u64,
    /// 总交换空间（页面数）
    pub totalswap: u64,
    /// 可用交换空间（页面数）
    pub freeswap: u64,
    /// 程序数
    pub procs: u16,
    /// 保留字段
    pub pad: u16,
    /// 总高端内存（页面数）
    pub totalhigh: u64,
    /// 可用高端内存（页面数）
    pub freehigh: u64,
    /// 内存单元大小
    pub mem_unit: u32,
}

impl SysInfo {
    /// 创建新的 SysInfo，初始化为 0
    pub const fn new() -> Self {
        SysInfo {
            uptime: 0,
            loads: [0; 3],
            totalram: 0,
            freeram: 0,
            sharedram: 0,
            bufferram: 0,
            totalswap: 0,
            freeswap: 0,
            procs: 0,
            pad: 0,
            totalhigh: 0,
            freehigh: 0,
            mem_unit: 1,
        }
    }

    /// 获取系统信息
    pub fn sysinfo() -> SysInfo {
        let mut info = SysInfo::new();

        // 运行时间（秒）= jiffies / HZ
        let jiffies = sched::jiffies();
        let hz = sched::task::HZ;
        info.uptime = (jiffies / hz) as i64;

        // 负载平均值（简化版本，这里设为 0）
        // TODO: 实现真实的负载计算
        info.loads = [0; 3];

        // 进程计数
        info.procs = count_tasks() as u16;

        // 内存信息（使用 mm 模块获取）
        // SAFETY: 只读内存统计
        unsafe {
            let (total, free, _, _) = mm_info();
            info.totalram = total;
            info.freeram = free;
        }

        // 交换空间（目前没有实现交换）
        info.totalswap = 0;
        info.freeswap = 0;

        // 共享内存和缓冲区（目前没有实现）
        info.sharedram = 0;
        info.bufferram = 0;

        // 高端内存（64 位架构没有高端内存）
        info.totalhigh = 0;
        info.freehigh = 0;

        // 内存单元大小
        info.mem_unit = 1;

        info
    }
}

/// 统计活动任务数量
fn count_tasks() -> usize {
    let mut count = 0;
    // SAFETY: 只读任务表
    unsafe {
        for i in 0..sched::NR_TASKS {
            let task = sched::task_ptr(i);
            if (*task).state != sched::task::TaskState::Unused {
                count += 1;
            }
        }
    }
    count
}

/// 获取内存统计信息
/// 返回 (total_pages, free_pages, shared_pages, buffer_pages)
unsafe fn mm_info() -> (u64, u64, u64, u64) {
    // TODO: 从 mm 模块获取真实的内存统计
    // 目前返回占位值
    (0, 0, 0, 0)
}

/// 初始化 info 模块
pub fn init() {
    let info = SysInfo::sysinfo();
    crate::sprintln!(
        "info: uptime={}s procs={} totalram={}KB freeram={}KB",
        info.uptime,
        info.procs,
        info.totalram * 4, // 假设页面大小 4KB
        info.freeram * 4
    );
}

// =============================================================================
// Self-Tests
// =============================================================================

/// 运行系统信息自检
pub fn selftest() {
    crate::sprintln!("--- info selftest ---");

    // 测试 SysInfo::new()
    let info = SysInfo::new();
    assert!(info.uptime == 0);
    assert!(info.procs == 0);

    // 测试 sysinfo() 返回有效数据
    let info = SysInfo::sysinfo();
    assert!(info.uptime >= 0);
    assert!(info.procs >= 1); // 至少 swapper 进程

    crate::sprintln!("info: sysinfo() uptime={}s procs={} -> ok", info.uptime, info.procs);
}
