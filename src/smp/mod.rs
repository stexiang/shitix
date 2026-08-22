//! SMP (Symmetric Multiprocessing) 支持模块
//!
//! 提供多核 CPU 支持，包括：
//! - LAPIC (Local APIC) MMIO 映射与寄存器访问
//! - AP 启动：16 位实模式蹦床（`boot/ap_trampoline.S`，物理 0x2000）
//!   + INIT-SIPI-SIPI 握手
//! - 自旋锁与「BSP 派工、全核并行执行」的分发器（[`run_on_all_cpus`]）
//!
//! 调度器仍是单核的：AP 启动后进入 [`smp_ap_main`] 的任务等待循环，
//! 只在 BSP 显式派工时参与计算，不参与进程调度。

pub mod tests;

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::mm::paging::{self, flags};

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
const APIC_SW_ENABLE: u32 = 0x100;

/// LVT 屏蔽位（bit 16）
const LVT_MASKED: u32 = 1 << 16;

/// IA32_APIC_BASE MSR：bit 8 表示本核是 BSP，bit 11 是 LAPIC 全局使能
const MSR_APIC_BASE: u32 = 0x1B;
const APIC_BASE_BSP: u64 = 1 << 8;
const APIC_BASE_ENABLE: u64 = 1 << 11;

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

// ICR 低位字段（Intel SDM Vol.3A 10.6.1）
const ICR_DELIVERY_SHIFT: u32 = 8;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_TRIG_LEVEL: u32 = 1 << 15;
/// bit 12：delivery status（只读，1 = send pending）
const ICR_STATUS_PENDING: u32 = 1 << 12;

/// 读取 LAPIC MMIO 寄存器
#[inline]
unsafe fn lapic_mmio_read(offset: u32) -> u32 {
    let base = unsafe { *core::ptr::addr_of!(LAPIC_BASE) };
    if base == 0 { return 0; }
    // SAFETY: LAPIC_BASE 由 smp_init 以 PCD（禁缓存）映射进内核页表后
    // 才非零，此时地址必然可访问。
    unsafe { core::ptr::read_volatile((base + offset as u64) as *const u32) }
}

/// 写入 LAPIC MMIO 寄存器
#[inline]
unsafe fn lapic_mmio_write(offset: u32, val: u32) {
    let base = unsafe { *core::ptr::addr_of!(LAPIC_BASE) };
    if base == 0 { return; }
    // SAFETY: 同 lapic_mmio_read。
    unsafe { core::ptr::write_volatile((base + offset as u64) as *mut u32, val) }
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

/// 全局 LAPIC MMIO 基址（虚拟地址，smp_init 里与物理地址恒等映射）
static mut LAPIC_BASE: u64 = 0;

/// APIC 是否已初始化
static APIC_INIT: AtomicBool = AtomicBool::new(false);

/// 在线 CPU 数量（含 BSP）
static CPU_COUNT: AtomicU32 = AtomicU32::new(1);

/// BSP 的 APIC ID
static BSP_APIC_ID: AtomicU32 = AtomicU32::new(0);

// ---- SMP 启动参数 ----

/// 支持的最大 CPU 数（含 BSP）。QEMU 默认 APIC ID 从 0 连续编号。
pub const MAX_CPUS: usize = 8;

/// 蹦床物理地址。低 1MB 是保留区（不进空闲链表），其中只有
/// 0x1000-0x3FFF 没被引导页表/内核镜像/机器参数占用，取后 4KB。
const TRAMPOLINE_PHYS: usize = 0x2000;

/// 蹦床二进制（构建系统由 boot/ap_trampoline.S 产出，先汇编后 cargo build）
const TRAMPOLINE_BIN: &[u8] = include_bytes!("../../target/boot/ap_trampoline.bin");

/// 蹦床页尾信箱偏移（与 ap_trampoline.S 的 .set 一致）
const TRAMP_OFF_MAGIC: usize = 0xF00;
const TRAMP_OFF_CR3: usize = 0xF08;
const TRAMP_OFF_STACK: usize = 0xF10;
const TRAMP_OFF_ENTRY: usize = 0xF18;
const TRAMP_OFF_CPU_ID: usize = 0xF20;
/// "SMPTRMP!"，BSP 拷完蹦床后校验用
const TRAMP_MAGIC: u64 = 0x2150_4D52_5450_4D53;

/// LAPIC MMIO 物理地址（QEMU/PC 默认）
const LAPIC_PHYS: usize = 0xFEE0_0000;

/// AP 内核栈虚拟基址：紧接高半区直接映射（PHYS_MAP_BASE+1GB）之上。
/// 只映进内核 PML4（AP 永远用内核 CR3），用户 PML4 看不到。
const AP_STACK_VBASE: usize = 0xffff_8000_4000_0000;
/// 每核栈 16KB（4 页）
const AP_STACK_PAGES: usize = 4;
/// 步进 32KB：16KB 栈 + 16KB 不映射的 guard 洞，栈溢出即缺页
const AP_STACK_STRIDE: usize = 0x8000;

/// 各核在线标志。`CPU_ONLINE[0]` 是 BSP，smp_init 一开始置位；
/// 其余由对应 AP 在 [`smp_ap_main`] 里自己置位，BSP 据此判断 SIPI 是否成功。
static CPU_ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

// ---- 并行任务分发器 ----
//
// BSP 派一个任务给所有在线核（含自己）执行：先写 JOB_FN/JOB_ARG，再把
// JOB_SEQ 加一作为「新一代任务」的信号；每个 AP 在自己的等待循环里看到
// 序号变化就取出函数执行，完成后把自己的 JOB_DONE 置成该序号。BSP 收齐
// 所有 JOB_DONE 后返回。

/// 任务函数指针：fn(cpu 逻辑号, arg)
static JOB_FN: AtomicUsize = AtomicUsize::new(0);
static JOB_ARG: AtomicUsize = AtomicUsize::new(0);
/// 任务代序号，单调递增；0 = 还没有派过工
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);
/// 每个核最后完成的代序号
static JOB_DONE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// 获取 LAPIC 基地址
pub fn get_lapic_base() -> u64 {
    // SAFETY: 只读一个 u64；写侧（set_lapic_base）只在 smp_init 早期发生。
    unsafe { *core::ptr::addr_of!(LAPIC_BASE) }
}

/// 设置 LAPIC 基地址
pub fn set_lapic_base(base: u64) {
    // SAFETY: 单核启动期独占写。
    unsafe { *core::ptr::addr_of_mut!(LAPIC_BASE) = base };
}

/// 读取 APIC 寄存器
pub fn lapic_read(reg: ApicReg) -> u32 {
    // SAFETY: 基址为 0 时安全返回 0；非零时代表映射已建好。
    unsafe { lapic_mmio_read(reg as u32) }
}

/// 写入 APIC 寄存器
pub fn lapic_write(reg: ApicReg, value: u32) {
    // SAFETY: 同 lapic_read。
    unsafe { lapic_mmio_write(reg as u32, value) };
}

/// 获取本地 APIC ID
pub fn lapic_id() -> u8 {
    ((lapic_read(ApicReg::Id) >> 24) & 0xFF) as u8
}

/// 获取 APIC 版本
pub fn lapic_version() -> u8 {
    (lapic_read(ApicReg::Version) & 0xFF) as u8
}

/// 启用本地 APIC（spurious vector = 0xFF）
pub fn lapic_enable() {
    let spurious = lapic_read(ApicReg::SpuriousIntVector);
    lapic_write(ApicReg::SpuriousIntVector, spurious | APIC_SW_ENABLE | 0xFF);
    APIC_INIT.store(true, Ordering::Release);
}

/// 发送 EOI (End Of Interrupt)
pub fn lapic_eoi() {
    lapic_write(ApicReg::Eoi, 0);
}

/// 设置任务优先级（0 = 接受所有中断）
pub fn lapic_set_tpr(tpr: u8) {
    lapic_write(ApicReg::TaskPriority, tpr as u32);
}

/// 获取在线 CPU 数量
pub fn get_cpu_count() -> u32 {
    CPU_COUNT.load(Ordering::Acquire)
}

/// 设置 CPU 数量（自检用）
pub fn set_cpu_count(count: u32) {
    CPU_COUNT.store(count, Ordering::Release);
}

/// 获取 BSP 的 APIC ID
pub fn get_bsp_cpu_id() -> u32 {
    BSP_APIC_ID.load(Ordering::Acquire)
}

/// 检查 APIC 是否已初始化
pub fn is_apic_initialized() -> bool {
    APIC_INIT.load(Ordering::Acquire)
}

/// 第 `cpu` 个逻辑核是否在线
pub fn is_cpu_online(cpu: usize) -> bool {
    cpu < MAX_CPUS && CPU_ONLINE[cpu].load(Ordering::Acquire)
}

/// 写 ICR 发一次 IPI。
///
/// 不轮询 delivery status（ICR 低 12 位）：该位在 QEMU/TCG 下对 MMIO
/// 读的返回值不可靠（表现为偶发永不清位、把 BSP 卡死在启动里），而
/// INIT/SIPI 的成功与否本来就要靠之后「AP 是否置位 CPU_ONLINE」来判断，
/// 轮询这个状态位不提供任何额外信息。
fn icr_send(apic_id: u8, icr_low: u32) {
    lapic_write(ApicReg::IcrHigh, (apic_id as u32) << 24);
    lapic_write(ApicReg::IcrLow, icr_low);
}

/// 发送 IPI (Inter-Processor Interrupt) 到指定 CPU
pub fn lapic_send_ipi(apic_id: u8, vector: u8, delivery_mode: DeliveryMode) {
    let icr_low = (vector as u32)
        | ((delivery_mode as u32) << ICR_DELIVERY_SHIFT)
        | ICR_LEVEL_ASSERT;
    icr_send(apic_id, icr_low);
}

/// 发送 INIT IPI 到指定 CPU（level-triggered assert，MP 规范）
pub fn lapic_send_init(apic_id: u8) {
    icr_send(
        apic_id,
        ((DeliveryMode::Init as u32) << ICR_DELIVERY_SHIFT) | ICR_LEVEL_ASSERT | ICR_TRIG_LEVEL,
    );
}

/// 发送 STARTUP IPI 到指定 CPU（edge，vector = 实模式启动页号）
pub fn lapic_send_startup(apic_id: u8, vector: u8) {
    icr_send(
        apic_id,
        (vector as u32) | ((DeliveryMode::Startup as u32) << ICR_DELIVERY_SHIFT) | ICR_LEVEL_ASSERT,
    );
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

/// 按 jiffies（100Hz 时钟滴答）等待 `ticks` 个滴答。
///
/// 启动 AP 的等待不能用「数 pause」的忙循环校准时间：QEMU/TCG 下宿主
/// 被限速时，忙循环可能几十倍快于真实时间，AP 的 vCPU 线程根本拿不到
/// 时间片；而 jiffies 由 PIT 中断驱动，wall-clock 恒定。smp_init 运行时
/// 调度器已初始化、中断已开，jiffies 会正常前进。
fn wait_ticks(ticks: u64) {
    let start = crate::sched::jiffies();
    while crate::sched::jiffies() - start < ticks {
        core::hint::spin_loop();
    }
}

// ---- QEMU fw_cfg（读真实 vCPU 数，避免探测不存在的 APIC ID） ----

const FW_CFG_SEL_PORT: u16 = 0x510;
const FW_CFG_DATA_PORT: u16 = 0x511;
const FW_CFG_SIGNATURE: u16 = 0x0000;
const FW_CFG_NB_CPUS: u16 = 0x0005;

unsafe fn outw(port: u16, val: u16) {
    // SAFETY: CPL=0，写 QEMU fw_cfg 选择器端口，无副作用于其他设备
    unsafe {
        core::arch::asm!("outw %ax, %dx", in("dx") port, in("ax") val,
                         options(nomem, nostack, preserves_flags, att_syntax));
    }
}

unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: CPL=0，读 fw_cfg 数据端口
    unsafe {
        core::arch::asm!("inb %dx, %al", in("dx") port, out("al") v,
                         options(nomem, nostack, preserves_flags, att_syntax));
    }
    v
}

/// 从 QEMU fw_cfg 读取配置的 vCPU 数；非 QEMU 环境返回 None。
fn fw_cfg_nb_cpus() -> Option<u16> {
    // SAFETY: 仅在 smp_init 调用一次；端口不存在时 inb 读到 0xFF，签名自然不匹配
    unsafe {
        outw(FW_CFG_SEL_PORT, FW_CFG_SIGNATURE);
        let mut sig = [0u8; 4];
        for b in &mut sig {
            *b = inb(FW_CFG_DATA_PORT);
        }
        if &sig != b"QEMU" {
            return None;
        }
        outw(FW_CFG_SEL_PORT, FW_CFG_NB_CPUS);
        let lo = inb(FW_CFG_DATA_PORT) as u16;
        let hi = inb(FW_CFG_DATA_PORT) as u16;
        Some(lo | (hi << 8))
    }
}

// ---- 自旋锁 ----

/// 基于 test-and-set 的自旋锁。SMP 下保护短的临界区用；
/// 持锁期间不要睡眠（调度器目前只在 BSP 上跑，AP 上也没有可睡的上下文）。
pub struct SpinLock {
    locked: AtomicBool,
}

impl SpinLock {
    pub const fn new() -> Self {
        Self { locked: AtomicBool::new(false) }
    }

    pub fn lock(&self) {
        loop {
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
            // 已有人持锁：先只读自旋，避免总线上刷 LOCK 前缀流量
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
    }

    pub fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }

    /// 是否处于锁定状态（调试用）
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }
}

// ---- SMP 启动 ----

/// 写蹦床信箱（BSP 侧经高半区直接映射写物理 0x2xxx）
unsafe fn tramp_write(offset: usize, val: u64) {
    // SAFETY: PHYS_MAP_BASE+0x2000 映射有效（低 1GB 都在直接映射内），
    // 信箱槽位独占，volatile 防优化。
    unsafe {
        core::ptr::write_volatile(
            (paging::PHYS_MAP_BASE + TRAMPOLINE_PHYS + offset) as *mut u64,
            val,
        )
    }
}

/// SMP 初始化：映射 LAPIC、使能 BSP LAPIC、拷贝蹦床、
/// 探测并按 INIT-SIPI-SIPI 顺序启动所有 AP。
///
/// 需要在 mm/desc/irq/sched 初始化之后、用户进程创建之前调用一次
/// （此时 current_pml4 就是内核引导 PML4，恒等映射完好）。
pub fn smp_init() {
    crate::sprintln!("SMP: initializing (trampoline at {:#x})", TRAMPOLINE_PHYS);

    // 确认本核是 BSP 且 LAPIC 全局使能（QEMU 复位后默认如此）
    let apic_base = rdmsr(MSR_APIC_BASE);
    if apic_base & APIC_BASE_ENABLE == 0 {
        // 全局使能位没置：开了再说
        wrmsr(MSR_APIC_BASE, apic_base | APIC_BASE_ENABLE);
    }
    BSP_APIC_ID.store(0, Ordering::Release); // 先用 0，LAPIC 可读后再修正

    let kernel_pml4 = paging::current_pml4();

    // 1. 映射 LAPIC MMIO（必须 PCD 禁缓存，否则 MMIO 读可能被缓存住）
    // SAFETY: kernel_pml4 有效；0xFEE00000 落在引导 PDPT 的第 3 项
    // （3-4GB），尚未使用，map_range 会新建 PD/PT，不撞大页。
    if !unsafe {
        paging::map_range(
            kernel_pml4,
            LAPIC_PHYS,
            LAPIC_PHYS,
            0x1000,
            flags::KERNEL | flags::PCD | flags::PWT,
        )
    } {
        crate::sprintln!("SMP: failed to map LAPIC MMIO, stay uniprocessor");
        return;
    }
    set_lapic_base(LAPIC_PHYS as u64);

    // 2. 使能 BSP 的 LAPIC，记录真实 BSP APIC ID
    lapic_enable();
    lapic_set_tpr(0);
    let bsp_id = lapic_id();
    BSP_APIC_ID.store(bsp_id as u32, Ordering::Release);
    CPU_ONLINE[0].store(true, Ordering::Release);
    crate::sprintln!(
        "SMP: BSP APIC id={} version={:#x}",
        bsp_id,
        lapic_version()
    );

    // 3. 拷贝蹦床到 0x2000 并填 magic
    // SAFETY: 0x2000 在低 1MB 保留区，不与任何内核数据冲突；直接映射可写。
    unsafe {
        core::ptr::copy_nonoverlapping(
            TRAMPOLINE_BIN.as_ptr(),
            (paging::PHYS_MAP_BASE + TRAMPOLINE_PHYS) as *mut u8,
            TRAMPOLINE_BIN.len(),
        );
    }
    // SAFETY: 蹦床页已拷入，直接映射可写
    unsafe { tramp_write(TRAMP_OFF_MAGIC, TRAMP_MAGIC) };
    // SAFETY: 同上的映射
    let magic = unsafe {
        core::ptr::read_volatile((paging::PHYS_MAP_BASE + TRAMPOLINE_PHYS + TRAMP_OFF_MAGIC)
            as *const u64)
    };
    if magic != TRAMP_MAGIC {
        crate::sprintln!("SMP: trampoline magic mismatch ({:#x}), stay uniprocessor", magic);
        return;
    }

    // 4. 顺序启动 AP。QEMU 默认把 vCPU 的 APIC ID 从 0 连续编号，所以
    //    用 fw_cfg 拿到确切 vCPU 数后只需启动 1..n-1；拿不到（非 QEMU）
    //    才退回全范围探测。这避免了对不存在的核浪费超时等待（TCG 限速
    //    下每个超时都是秒级的）。
    let nb_cpus = fw_cfg_nb_cpus();
    let probe_max = match nb_cpus {
        Some(n) if (1..=MAX_CPUS as u16).contains(&n) => n,
        Some(n) => {
            crate::sprintln!("SMP: fw_cfg nb_cpus={} out of range, probe all", n);
            MAX_CPUS as u16
        }
        None => MAX_CPUS as u16,
    };
    let mut logical = 1usize;
    for apic_id in 0..(probe_max as u8) {
        if apic_id == bsp_id {
            continue;
        }
        // SAFETY: 蹦床已就位，信箱逐核填写；每次只启动一个核。
        if unsafe { start_one_ap(kernel_pml4, apic_id, logical) } {
            crate::sprintln!("SMP: CPU{} online (APIC id={})", logical, apic_id);
            logical += 1;
        }
    }
    CPU_COUNT.store(logical as u32, Ordering::Release);
    crate::sprintln!("SMP: {} CPU(s) online", logical);
}

/// 启动单个 AP：分配并映射栈、填信箱、INIT-SIPI-SIPI、等 ready。
///
/// 返回 true 表示该 AP 成功报到。
///
/// # Safety
/// 仅在 [`smp_init`] 里串行调用。
unsafe fn start_one_ap(kernel_pml4: usize, apic_id: u8, logical: usize) -> bool {
    // 分配 4 个物理页，映射到本 AP 的高半区栈位（物理页不要求连续，
    // 虚拟连续即可）。
    let stack_vbase = AP_STACK_VBASE + logical * AP_STACK_STRIDE;
    let mut pages = [0usize; AP_STACK_PAGES];
    for (i, slot) in pages.iter_mut().enumerate() {
        let p = crate::mm::page_alloc::get_free_page();
        if p == 0 {
            crate::sprintln!("SMP: OOM for AP{} stack", logical);
            for q in pages.iter().take(i) {
                crate::mm::free_page(*q);
            }
            return false;
        }
        *slot = p;
        // SAFETY: stack_vbase 区域独占未用；p 是刚分配的有效物理页
        if !unsafe {
            paging::map_page(
                kernel_pml4,
                stack_vbase + i * crate::mm::PAGE_SIZE,
                p,
                flags::KERNEL,
            )
        } {
            crate::sprintln!("SMP: failed to map AP{} stack", logical);
            for q in pages.iter().take(i + 1) {
                crate::mm::free_page(*q);
            }
            return false;
        }
    }
    let stack_top = (stack_vbase + AP_STACK_PAGES * crate::mm::PAGE_SIZE) as u64;
    // 栈顶 16 字节对齐（SysV ABI 入口要求）
    let stack_top = stack_top & !0xF;

    // 填信箱：CR3、栈、入口、逻辑 CPU 号
    // SAFETY: 蹦床页已就位；下面是逐槽 volatile 写。
    unsafe {
        tramp_write(TRAMP_OFF_CR3, kernel_pml4 as u64);
        tramp_write(TRAMP_OFF_STACK, stack_top);
        tramp_write(TRAMP_OFF_ENTRY, smp_ap_main as *const () as u64);
        tramp_write(TRAMP_OFF_CPU_ID, logical as u64);
    }
    // 信箱要对 SIPI 后醒来的核可见：x86 TSO 下普通写已按序，
    // 这里再补一个 SeqCst 栅栏防编译器重排。
    core::sync::atomic::fence(Ordering::SeqCst);

    // INIT → 10ms → SIPI → 200us → SIPI（MP 规范 B.4）。
    // 等待全部用 jiffies（wall-clock 恒定），不能用数空转的忙等待：
    // TCG 限速宿主上忙循环可能几十倍快于真实时间，AP 的 vCPU 线程
    // 拿不到时间片就永远等不到。
    let vector = (TRAMPOLINE_PHYS >> 12) as u8;
    lapic_send_init(apic_id);
    wait_ticks(2); // ~20ms >= 规范的 10ms
    lapic_send_startup(apic_id, vector);
    wait_ticks(1);
    lapic_send_startup(apic_id, vector);
    // 等 AP 报到，上限 2 秒（jiffies 200 滴答）
    let deadline = crate::sched::jiffies() + 200;
    let mut ok = false;
    while crate::sched::jiffies() < deadline {
        if CPU_ONLINE[logical].load(Ordering::Acquire) {
            ok = true;
            break;
        }
        core::hint::spin_loop();
    }

    if !ok {
        crate::sprintln!("SMP: APIC id={} did not respond", apic_id);
        // 没起来的 AP 永远不会用到这段栈，回收物理页
        for (i, p) in pages.iter().enumerate() {
            // SAFETY: 栈映射只建在内核 PML4，AP 未启动故无人使用
            let _ = unsafe {
                paging::unmap_page(kernel_pml4, stack_vbase + i * crate::mm::PAGE_SIZE)
            };
            crate::mm::free_page(*p);
        }
    }
    ok
}

/// 读 MSR
fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: CPL=0 下 rdmsr 合法；调用的 MSR 都是存在的架构 MSR。
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((hi as u64) << 32) | lo as u64
}

/// 写 MSR
fn wrmsr(msr: u32, val: u64) {
    // SAFETY: 同 rdmsr。
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") val as u32,
            in("edx") (val >> 32) as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// AP 的 Rust 入口（蹦床把逻辑 CPU 号放在 rdi 跳进来）。
///
/// 完成本核 LAPIC 初始化后进入任务等待循环：轮询 JOB_SEQ，BSP 派工
/// 时执行并上报完成。不返回、不睡眠、不碰调度器。
extern "C" fn smp_ap_main(cpu_id: u64) -> ! {
    let cpu = cpu_id as usize;
    // SAFETY: 蹦床已把内核 CR3/栈/本函数地址交给我们；BSP 的 GDT/IDT
    // 早已建好。中断在本核保持关闭（蹦床 cli 后没开），lidt 只是
    // 让异常有个去处（真出异常说明 AP 路径有 bug）。
    unsafe { crate::desc::ap_load_tables() };

    // 本核 LAPIC：使能 + 清 TPR，屏蔽 LINT0/LINT1/Timer（PIC 的
    // ExtINT 只该去 BSP，AP 不接外设中断）
    lapic_enable();
    lapic_set_tpr(0);
    lapic_write(ApicReg::LvtLint0, LVT_MASKED);
    lapic_write(ApicReg::LvtLint1, LVT_MASKED);
    lapic_write(ApicReg::LvtTimer, LVT_MASKED);
    lapic_eoi();

    if cpu >= MAX_CPUS {
        // 逻辑号越界是 BSP 侧 bug；停在这里别污染共享状态
        loop { unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) } }
    }

    // 报到
    CPU_ONLINE[cpu].store(true, Ordering::Release);

    // 任务等待循环
    let mut last_seq = JOB_SEQ.load(Ordering::Acquire);
    loop {
        let seq = JOB_SEQ.load(Ordering::Acquire);
        if seq != last_seq {
            let f = JOB_FN.load(Ordering::Acquire);
            let arg = JOB_ARG.load(Ordering::Acquire);
            if f != 0 {
                // SAFETY: f 由 BSP 在派工前置入，一定是合法的
                // extern "C" fn(usize, usize)（见 run_on_all_cpus）。
                let job: extern "C" fn(usize, usize) =
                    unsafe { core::mem::transmute(f as *const ()) };
                job(cpu, arg);
            }
            JOB_DONE[cpu].store(seq, Ordering::Release);
            last_seq = seq;
        }
        core::hint::spin_loop();
    }
}

/// 派一个任务到所有在线核（含 BSP 自己，cpu 号 0）并行执行，
/// 阻塞等待全部完成后返回。
///
/// 调度器本身仍是单核的，这是给「多核并行计算」用的显式分发接口：
/// `job` 在每个核上以 `(cpu 逻辑号, arg)` 调用一次。
pub fn run_on_all_cpus(job: extern "C" fn(usize, usize), arg: usize) {
    let ncpu = get_cpu_count() as usize;
    JOB_FN.store(job as usize, Ordering::Relaxed);
    JOB_ARG.store(arg, Ordering::Relaxed);
    // 保证 FN/ARG 先于 SEQ 被 AP 看到
    core::sync::atomic::fence(Ordering::SeqCst);
    let seq = JOB_SEQ.fetch_add(1, Ordering::SeqCst).wrapping_add(1);

    // BSP 自己也跑一份（cpu 0）
    job(0, arg);
    JOB_DONE[0].store(seq, Ordering::Release);

    // 收齐所有在线 AP 的完成回执
    for cpu in 1..ncpu {
        if !is_cpu_online(cpu) {
            continue;
        }
        while JOB_DONE[cpu].load(Ordering::Acquire) != seq {
            core::hint::spin_loop();
        }
    }
}

/// 获取当前 CPU 的 APIC ID
pub fn current_apic_id() -> u8 {
    lapic_id()
}

/// 检查是否运行在 BSP（读 IA32_APIC_BASE MSR 的 BSP 标志位，
/// 不依赖「BSP 的 APIC ID 一定是 0」这类平台假设）
pub fn is_bsp() -> bool {
    rdmsr(MSR_APIC_BASE) & APIC_BASE_BSP != 0
}
