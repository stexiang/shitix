//! SMP 自检。
//!
//! 覆盖：APIC 寄存器定义、CPU 状态管理、LAPIC 检测与寄存器读写、
//! IPI 发送（无真实 AP 时也能测 ICR 轮询）、LAPIC 定时器配置，
//! 以及 AP 启动后的多核并行计算正确性（[`parallel_selftest`]）。

use super::*;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// 主入口：运行所有 SMP 自检项。
/// 返回 `true` 表示全部通过。
#[inline(never)]
pub unsafe fn selftest() -> bool {
    let mut ok = true;
    let mut check = |cond: bool, tag: &str| {
        if !cond { ok = false; crate::sprintln!("smp: {} FAIL", tag); }
    };

    // 1. APIC 寄存器偏移常量
    check(ApicReg::Id as u32 == 0x020, "ApicReg::Id");
    check(ApicReg::Version as u32 == 0x030, "ApicReg::Version");
    check(ApicReg::Eoi as u32 == 0x0B0, "ApicReg::Eoi");
    check(ApicReg::SpuriousIntVector as u32 == 0x0F0, "SpuriousIntVector");
    check(ApicReg::IcrLow as u32 == 0x300, "ApicReg::IcrLow");
    check(ApicReg::IcrHigh as u32 == 0x310, "ApicReg::IcrHigh");
    check(ApicReg::LvtTimer as u32 == 0x320, "ApicReg::LvtTimer");
    check(ApicReg::TimerInitialCount as u32 == 0x380, "ApicReg::TimerInitialCount");
    check(ApicReg::TimerCurrentCount as u32 == 0x390, "ApicReg::TimerCurrentCount");
    check(ApicReg::TimerDivideConfig as u32 == 0x3E0, "ApicReg::TimerDivideConfig");
    check(ApicReg::LvtError as u32 == 0x370, "ApicReg::LvtError");
    check(ApicReg::LvtLint0 as u32 == 0x350, "ApicReg::LvtLint0");
    check(ApicReg::LvtLint1 as u32 == 0x360, "ApicReg::LvtLint1");

    // 2. DeliveryMode 枚举值
    check(DeliveryMode::Fixed as u32 == 0, "DeliveryMode::Fixed");
    check(DeliveryMode::Init as u32 == 5, "DeliveryMode::Init");
    check(DeliveryMode::Startup as u32 == 6, "DeliveryMode::Startup");

    // 3. CpuInfo 构造与状态
    let mut ci = CpuInfo::new(42);
    check(ci.apic_id == 42, "CpuInfo::apic_id");
    check(ci.logical_id == 0, "CpuInfo::logical_id default");
    check(ci.state == CpuState::Uninitialized, "CpuInfo::state default");
    ci.state = CpuState::Ready;
    check(ci.state == CpuState::Ready, "CpuInfo::state Ready");
    ci.state = CpuState::Running;
    check(ci.state == CpuState::Running, "CpuInfo::state Running");
    ci.state = CpuState::Stopped;
    check(ci.state == CpuState::Stopped, "CpuInfo::state Stopped");

    // 4. CPU count 管理
    set_cpu_count(1);
    check(get_cpu_count() == 1, "CPU_COUNT default 1");
    set_cpu_count(4);
    check(get_cpu_count() == 4, "CPU_COUNT set 4");
    set_cpu_count(8);
    check(get_cpu_count() == 8, "CPU_COUNT count set 8");
    set_cpu_count(1);

    // 5. BSP CPU ID
    check(get_bsp_cpu_id() == 0, "BSP_CPU_ID default 0");

    // 6. LAPIC 基地址存取
    let old_base = get_lapic_base();
    set_lapic_base(0xFEE0_0000u64);
    check(get_lapic_base() == 0xFEE0_0000, "LAPIC_BASE set/read");
    set_lapic_base(old_base);

    // 7. 空基址防护：LAPIC_BASE==0 时读/写不应 panic
    set_lapic_base(0);
    lapic_eoi();
    check(lapic_id() == 0, "lapic_id no-LAPIC returns 0");
    check(lapic_version() == 0, "lapic_version no-LAPIC returns 0");
    check(!is_apic_initialized(), "APIC_INIT false");
    set_lapic_base(old_base);

    // 8. 自旋锁基本语义（本核内）
    let lock = SpinLock::new();
    check(!lock.is_locked(), "SpinLock initial unlocked");
    lock.lock();
    check(lock.is_locked(), "SpinLock locked");
    lock.unlock();
    check(!lock.is_locked(), "SpinLock unlocked");

    ok
}

// ---- 并行计算自检 ----

/// 各核算出的部分和
static PAR_RESULTS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// 本次参与计算的核数（BSP 派工前写入）
static PAR_NCPUS: AtomicUsize = AtomicUsize::new(1);
/// 求和规模
const PAR_N: u64 = 4_000_000;

/// 被求和函数：确定性强、每步便宜、结果 < 1_000_003 保证总和不溢出 u64
fn par_f(i: u64) -> u64 {
    (i * i + 3 * i + 7) % 1_000_003
}

/// 派到每个核上的 worker：算自己那一截的部分和
extern "C" fn par_job(cpu: usize, _arg: usize) {
    let ncpu = PAR_NCPUS.load(Ordering::Relaxed) as u64;
    let chunk = PAR_N / ncpu;
    let start = cpu as u64 * chunk;
    let end = if cpu as u64 + 1 == ncpu { PAR_N } else { start + chunk };
    let mut s = 0u64;
    for i in start..end {
        s += par_f(i);
    }
    PAR_RESULTS[cpu].store(s, Ordering::Release);
}

/// 多核并行计算自检：所有在线核（含 BSP）各算一段
/// `sum (i*i + 3i + 7) % 1000003`，汇总后与 BSP 单核算出的参考值对照。
/// 单核环境（无 AP）同样成立：唯一 worker 就是 BSP 自己。
pub fn parallel_selftest() -> bool {
    let ncpu = get_cpu_count() as usize;
    PAR_NCPUS.store(ncpu, Ordering::Relaxed);
    for r in &PAR_RESULTS {
        r.store(0, Ordering::Relaxed);
    }

    run_on_all_cpus(par_job, 0);

    let mut par = 0u64;
    for r in PAR_RESULTS.iter().take(ncpu) {
        par += r.load(Ordering::Relaxed);
    }

    let mut serial = 0u64;
    for i in 0..PAR_N {
        serial += par_f(i);
    }

    let ok = par == serial;
    crate::sprintln!(
        "smp parallel: {} CPU(s), sum={} ref={} {}",
        ncpu,
        par,
        serial,
        if ok { "ok" } else { "FAIL" }
    );
    ok
}

// ---- 调度偷取自检：AP 是否真的从任务环里偷到一个内核线程并执行 ----

static STEAL_DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static STEAL_CPU: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(u64::MAX);

fn steal_worker(_arg: u64) {
    STEAL_CPU.store(crate::sched::this_cpu() as u64, Ordering::Release);
    STEAL_DONE.store(true, Ordering::Release);
}

/// 多核调度自检：创建一个内核线程，BSP 不调度它（忙等），验证它被
/// 某个 AP 从任务环偷走并执行（worker 汇报 this_cpu() > 0）。
/// 单核环境直接跳过（没有 AP 可偷）。
pub fn sched_steal_selftest() -> bool {
    let ncpu = get_cpu_count() as usize;
    if ncpu < 2 {
        crate::sprintln!("smp sched: single CPU, skip steal test");
        return true;
    }
    STEAL_DONE.store(false, Ordering::Relaxed);
    STEAL_CPU.store(u64::MAX, Ordering::Relaxed);
    let nr = match crate::sched::kernel_thread("stealtest", steal_worker, 0, 15) {
        Ok(n) => n,
        Err(_) => {
            crate::sprintln!("smp sched: kernel_thread failed");
            return false;
        }
    };
    let _ = nr;
    // BSP 忙等不调度：只有自己不调 schedule()，线程才必然由 AP 偷走。
    // 开中断让 jiffies 走动以做超时。
    // SAFETY: IDT/PIC 就绪。
    unsafe { crate::irq::sti() };
    let deadline = crate::sched::jiffies() + 200;
    while !STEAL_DONE.load(Ordering::Acquire) {
        if crate::sched::jiffies() > deadline {
            crate::sprintln!("smp sched: steal TIMEOUT");
            return false;
        }
        core::hint::spin_loop();
    }
    let cpu = STEAL_CPU.load(Ordering::Acquire);
    let ok = cpu > 0 && (cpu as usize) < ncpu;
    crate::sprintln!(
        "smp sched: worker ran on cpu{} {}",
        cpu,
        if ok { "ok" } else { "FAIL (expected AP)" }
    );
    ok
}
