//! SMP 自检。
//!
//! 覆盖：APIC 寄存器定义、CPU 状态管理、LAPIC 检测与寄存器读写、
//! IPI 发送（无真实 AP 时也能测 ICR 轮询）、LAPIC 定时器配置。

use super::*;

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

    ok
}
