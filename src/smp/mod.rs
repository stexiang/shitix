//! SMP (Symmetric Multiprocessing) 支持模块
//! 
//! 提供多核 CPU 支持，包括：
//! - APIC (Advanced Programmable Interrupt Controller) 管理
//! - 多核启动和同步
//! - CPU 热插拔支持

/// APIC 寄存器偏移
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum ApicReg {
    Id = 0x020,
    Version = 0x030,
    TaskPriority = 0x080,
    ArbitrationPriority = 0x090,
    ProcessorPriority = 0x0A0,
    Eoi = 0x0B0,
    LogicalDest = 0x0D0,
    DestinationFormat = 0x0E0,
    SpuriousIntVector = 0x0F0,
    Enable = 0x100,
    TriggerMode = 0x108,
    IrqPinAssertion = 0x110,
    IrqPin = 0x118,
    ErrorStatus = 0x280,
    LvtCorrectedMachinceCheck = 0x2F0,
    IcrLow = 0x300,
    IcrHigh = 0x310,
    LvtTimer = 0x320,
    LvtThermal = 0x330,
    LvtPerfMon = 0x340,
    LvtLint0 = 0x350,
    LvtLint1 = 0x360,
    LvtError = 0x370,
    TimerInitialCount = 0x380,
    TimerCurrentCount = 0x390,
    TimerDivideConfig = 0x3E0,
}

/// APIC 寄存器掩码
const APIC_ENABLE: u32 = 0x100;
const APIC_FOCUS_DISABLED: u32 = 0x200;
const APIC_SW_ENABLE: u32 = 0x100;

/// 投递模式
#[derive(Debug, Clone, Copy)]
pub enum DeliveryMode {
    Fixed = 0,
    LowestPriority = 1,
    Smi = 2,
    Nmi = 4,
    Init = 5,
    Startup = 6,
}

/// 读取 32 位端口
#[inline]
unsafe fn inl(port: u16) -> u32 {
    let result: u32;
    core::arch::asm!("inl %dx, %eax", in("dx") port, out("eax") result, options(nostack));
    result
}

/// 写入 32 位端口
#[inline]
unsafe fn outl(port: u16, val: u32) {
    core::arch::asm!("outl %eax, %dx", in("dx") port, in("eax") val, options(nomem, nostack));
}

/// CPU 状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CpuState {
    Uninitialized,
    Initializing,
    Ready,
    Running,
    Stopped,
}

/// CPU 信息
#[repr(C)]
pub struct CpuInfo {
    pub apic_id: u8,
    pub logical_id: u8,
    pub state: CpuState,
    pub stack_pointer: u64,
    pub kernel_stack: u64,
}

impl CpuInfo {
    pub const fn new(apic_id: u8) -> Self {
        Self {
            apic_id,
            logical_id: 0,
            state: CpuState::Uninitialized,
            stack_pointer: 0,
            kernel_stack: 0,
        }
    }
}

/// 全局 APIC 基地址
static mut LAPIC_BASE: u64 = 0;

/// APIC 是否已初始化
static mut APIC_INIT: bool = false;

/// CPU 数量
static mut CPU_COUNT: u32 = 1;

/// 当前 CPU ID (BSP)
static mut BSP_CPU_ID: u32 = 0;

/// 获取 APIC 基地址
pub fn get_lapic_base() -> u64 {
    unsafe { LAPIC_BASE }
}

/// 设置 APIC 基地址
pub fn set_lapic_base(base: u64) {
    unsafe { LAPIC_BASE = base };
}

/// 读取 APIC 寄存器
pub fn lapic_read(reg: ApicReg) -> u32 {
    let base = unsafe { LAPIC_BASE };
    if base == 0 {
        return 0;
    }
    unsafe { inl((base + reg as u64) as u16) }
}

/// 写入 APIC 寄存器
pub fn lapic_write(reg: ApicReg, value: u32) {
    let base = unsafe { LAPIC_BASE };
    if base == 0 {
        return;
    }
    unsafe { outl((base + reg as u64) as u16, value) };
}

/// 获取本地 APIC ID
pub fn lapic_id() -> u8 {
    ((lapic_read(ApicReg::Id) >> 24) & 0xFF) as u8
}

/// 获取 APIC 版本
pub fn lapic_version() -> u8 {
    (lapic_read(ApicReg::Version) & 0xFF) as u8
}

/// 启用 APIC
pub fn lapic_enable() {
    let spurious = lapic_read(ApicReg::SpuriousIntVector);
    lapic_write(ApicReg::SpuriousIntVector, spurious | APIC_SW_ENABLE | 0xFF);
    unsafe { APIC_INIT = true; }
}

/// 发送 EOI (End Of Interrupt)
pub fn lapic_eoi() {
    lapic_write(ApicReg::Eoi, 0);
}

/// 设置任务优先级
pub fn lapic_set_tpr(tpr: u8) {
    lapic_write(ApicReg::TaskPriority, tpr as u32);
}

/// 获取 CPU 数量
pub fn get_cpu_count() -> u32 {
    unsafe { CPU_COUNT }
}

/// 设置 CPU 数量
pub fn set_cpu_count(count: u32) {
    unsafe { CPU_COUNT = count };
}

/// 获取 BSP CPU ID
pub fn get_bsp_cpu_id() -> u32 {
    unsafe { BSP_CPU_ID }
}

/// 检查 APIC 是否已初始化
pub fn is_apic_initialized() -> bool {
    unsafe { APIC_INIT }
}

/// 发送 IPI (Inter-Processor Interrupt) 到指定 CPU
pub fn lapic_send_ipi(apic_id: u8, vector: u8, delivery_mode: DeliveryMode) {
    let icr_low = (vector as u32)
        | ((delivery_mode as u32) << 8)
        | (1 << 14)  // Trigger mode level
        | (1 << 15)  // Assert
        | (1 << 12); // Physical mode
    
    let icr_high = (apic_id as u32) << 24;
    
    // 设置目标
    lapic_write(ApicReg::IcrHigh, icr_high);
    // 发送
    lapic_write(ApicReg::IcrLow, icr_low);
    
    // 等待发送完成
    while (lapic_read(ApicReg::IcrLow) & (1 << 12)) != 0 {
        core::hint::spin_loop();
    }
}

/// 发送 INIT IPI 到指定 CPU
pub fn lapic_send_init(apic_id: u8) {
    lapic_send_ipi(apic_id, 0, DeliveryMode::Init);
}

/// 发送 STARTUP IPI 到指定 CPU
pub fn lapic_send_startup(apic_id: u8, vector: u8) {
    lapic_send_ipi(apic_id, vector, DeliveryMode::Startup);
}

/// 配置本地 APIC LINT0 (用于 ExtINT 或 NMI)
pub fn lapic_configure_lint0(edge_triggered: bool, active_high: bool, delivery_mode: DeliveryMode) {
    let mut value = delivery_mode as u32;
    if !edge_triggered {
        value |= 0x8000; // Level trigger
    }
    if !active_high {
        value |= 0x2000; // Active low
    }
    lapic_write(ApicReg::LvtLint0, value);
}

/// 配置 LINT1
pub fn lapic_configure_lint1(edge_triggered: bool, active_high: bool, delivery_mode: DeliveryMode) {
    let mut value = delivery_mode as u32;
    if !edge_triggered {
        value |= 0x8000;
    }
    if !active_high {
        value |= 0x2000;
    }
    lapic_write(ApicReg::LvtLint1, value);
}

/// 配置错误 LVT
pub fn lapic_configure_error(vector: u8) {
    lapic_write(ApicReg::LvtError, vector as u32);
}

/// 配置计时器 LVT
pub fn lapic_configure_timer(vector: u8, periodic: bool) {
    let mut value = vector as u32 | (1 << 17); // Enable
    if periodic {
        value |= 0x20000; // Periodic
    }
    lapic_write(ApicReg::LvtTimer, value);
}

/// 设置计时器分频
pub fn lapic_set_timer_divide(divide: u8) {
    let value = match divide {
        2 => 0b0000,
        4 => 0b0001,
        8 => 0b0010,
        16 => 0b0011,
        32 => 0b1000,
        64 => 0b1001,
        128 => 0b1010,
        1 | _ => 0b1011,
    };
    lapic_write(ApicReg::TimerDivideConfig, value);
}

/// 设置计时器初始计数
pub fn lapic_set_timer_initial(count: u32) {
    lapic_write(ApicReg::TimerInitialCount, count);
}

/// 获取计时器当前计数
pub fn lapic_get_timer_current() -> u32 {
    lapic_read(ApicReg::TimerCurrentCount)
}

/// 简单的微秒级延时
fn delay_us(us: u64) {
    // 简单的忙等待，基于 CPU 循环
    // 在实际系统中应该使用 HPET 或 PIT
    let cycles_per_us = 2000; // 假设 2GHz CPU
    let cycles = us * cycles_per_us;
    let mut counter = 0u64;
    while counter < cycles {
        core::hint::spin_loop();
        counter += 1;
    }
}

/// SMP 初始化
/// 
/// 初始化多核支持。需要在物理内存映射完成后调用。
pub fn smp_init() {
    crate::pr_info!("SMP: Initializing...");
    
    // 获取 BSP 的 APIC ID
    let bsp_apic_id = lapic_id();
    crate::pr_info!("SMP: BSP APIC ID = {}", bsp_apic_id);
    
    unsafe {
        BSP_CPU_ID = 0;  // BSP is always CPU 0
        CPU_COUNT = 1;
    }
    
    // 启用 APIC
    lapic_enable();
    crate::pr_info!("SMP: Local APIC enabled");
    
    // 配置 LINT1 为 NMI
    lapic_configure_lint1(true, true, DeliveryMode::Nmi);
    
    crate::pr_info!("SMP: Initialization complete, running on {} CPU(s)", get_cpu_count());
}

/// 启动应用处理器 (AP)
/// 
/// 启动指定 APIC ID 的应用处理器。
/// 
/// # Safety
/// 需要在适当的上下文中调用，有严格的内存和同步要求。
pub unsafe fn smp_start_cpu(apic_id: u8, start_vector: u64, _stack: u64) {
    crate::pr_info!("SMP: Starting CPU with APIC ID {}", apic_id);
    
    // 发送 INIT IPI
    lapic_send_init(apic_id);
    
    // 等待 10ms
    delay_us(10_000);
    
    // 发送 Startup IPI
    let vector = ((start_vector >> 12) & 0xFF) as u8;
    lapic_send_startup(apic_id, vector);
    
    // 等待 200us
    delay_us(200);
    
    // 再次发送 Startup (根据 Intel 规范)
    lapic_send_startup(apic_id, vector);
    
    crate::pr_info!("SMP: Startup IPI sent to CPU {}", apic_id);
}

/// 获取当前 CPU 的 APIC ID
pub fn current_apic_id() -> u8 {
    lapic_id()
}

/// 检查是否运行在 BSP
pub fn is_bsp() -> bool {
    current_apic_id() == unsafe { 
        // 获取 BSP APIC ID 的方式：通常在 MADT 表中标记
        // 这里简化处理，假设 BSP 的 APIC ID 是配置时记录的
        0  // TODO: 正确实现
    }
}

/// 处理器计数（自检）
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apic_registers() {
        // 测试 APIC 寄存器定义
        assert_eq!(ApicReg::Id as u32, 0x020);
        assert_eq!(ApicReg::Eoi as u32, 0x0B0);
        assert_eq!(ApicReg::IcrLow as u32, 0x300);
    }
}
