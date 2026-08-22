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

        // 负载平均值：以「可运行任务数」为底做 1/5/15 分钟定点指数平均
        // （定点 11 位，公式同内核 CALC_LOAD：load = load*(1-exp) + n*exp，
        // EXP_1 = 1884/2048, EXP_5 = 2014/2048, EXP_15 = 2037/2048）。
        // 每 5 秒（5*HZ tick）采样一次。
        info.loads = update_loads();

        // 进程计数
        info.procs = count_tasks() as u16;

        // 内存信息（真实页分配器统计）
        let (total, free) = mm_info();
        info.totalram = total;
        info.freeram = free;

        // 交换空间（页为单位）
        info.totalswap = crate::mm::swap::total_slots() as u64;
        info.freeswap = crate::mm::swap::nr_free_slots() as u64;

        // 共享内存没单独记账；缓冲区缓存头数固定 NR_BUFFERS
        info.sharedram = 0;
        info.bufferram = (crate::fs::buffer::NR_BUFFERS * crate::fs::buffer::BLOCK_SIZE
            / crate::mm::PAGE_SIZE) as u64;

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

/// 获取内存统计信息。返回 (totalram, freeram)，单位页。
fn mm_info() -> (u64, u64) {
    let total = (crate::mm::page_alloc::high_memory() / crate::mm::PAGE_SIZE) as u64;
    let free = crate::mm::page_alloc::nr_free_pages() as u64;
    (total, free)
}

/// 可运行任务数（含当前任务）。对应内核 `nr_running` 的采样。
fn nr_running() -> u64 {
    let mut n = 0;
    // SAFETY: 只读任务表。
    unsafe {
        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state == sched::task::TaskState::Running {
                n += 1;
            }
        }
    }
    n
}

/// 定点 11 位的负载平均。对应内核 `avenrun[]` + `calc_load()`。
static mut AVENRUN: [u64; 3] = [0; 3];
static mut LAST_LOAD_TICK: u64 = 0;

/// 每 5*HZ tick 采样一次 nr_running，更新三组指数平均。
/// 返回定点值 * 2048（Linux sysinfo 的 loads 语义：需 /65536 得真实值，
/// 这里直接给 SI_LOAD_SHIFT=16 的定点，与 struct sysinfo 的约定一致）。
fn update_loads() -> [u64; 3] {
    const FSHIFT: u32 = 11;
    const EXP_1: u64 = 1884; // 1 分钟
    const EXP_5: u64 = 2014; // 5 分钟
    const EXP_15: u64 = 2037; // 15 分钟
    let hz = sched::task::HZ;
    // SAFETY: sysinfo 查询上下文，单线程路径。
    unsafe {
        let now = sched::jiffies();
        let last = *core::ptr::addr_of!(LAST_LOAD_TICK);
        if now >= last + 5 * hz || *core::ptr::addr_of!(AVENRUN) == [0; 3] {
            *core::ptr::addr_of_mut!(LAST_LOAD_TICK) = now;
            let n = nr_running() << FSHIFT;
            let ar = &mut *core::ptr::addr_of_mut!(AVENRUN);
            for (i, exp) in [EXP_1, EXP_5, EXP_15].iter().enumerate() {
                let old = ar[i];
                // calc_load: load = old*exp + n*(FIXED_1-exp)，再右移 FSHIFT
                ar[i] = (old * exp + n * ((1 << FSHIFT) - exp)) >> FSHIFT;
            }
        }
        let ar = *core::ptr::addr_of!(AVENRUN);
        // 转成 sysinfo 的 SI_LOAD_SHIFT=16 定点
        [ar[0] << 5, ar[1] << 5, ar[2] << 5]
    }
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
