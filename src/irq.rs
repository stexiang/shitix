//! 硬件中断（IRQ）与 8259A PIC。对应 linux-1.0.9 的 `kernel/irq.c`。
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`init`] | `irq.c:init_IRQ()` + `head.S` 里没有的 PIC 重映射 |
//! | [`request_irq`] / [`free_irq`] | 同名函数 |
//! | [`do_irq`] | `irq.c:do_IRQ()`（+ `do_fast_IRQ` 的合并） |
//! | [`disable_irq`] / [`enable_irq`] | 同名函数（含 `cache_21`/`cache_A1`） |
//! | [`do_bottom_half`] | 同名函数 |
//! | [`INTR_COUNT`] | 全局 `intr_count` |
//!
//! 一处必要的差异：**原版不重映射 PIC**。32 位保护模式下 IRQ0-15 被
//! BIOS 映射到向量 8-15 和 0x70-0x77，与 CPU 异常向量冲突，原版
//! `boot/setup.S` 里有一段 `outb 0x11,0x20 ...` 把它们移到 0x20 起。
//! 我们的 `setup.S` 没做这件事（当时还不需要中断），所以在这里做。

use crate::klib::errno::{EBUSY, EINVAL, KResult};
use crate::klib::printk::Level;
use crate::traps::PtRegs;

/// IRQ 线数量。对应原版 `for (i = 0; i < 16 ; i++)`。
pub const NR_IRQS: usize = 16;

/// PIC 重映射后 IRQ0 对应的向量号。原版 `init_IRQ` 里的 `0x20+i`。
pub const IRQ_BASE: usize = 0x20;

// 8259A 的四个端口。原版散落在 irq.c 各处的 0x20/0x21/0xA0/0xA1。
const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// ICW1：需要 ICW4，级联模式，边沿触发
const ICW1_INIT: u8 = 0x11;
/// ICW4：8086 模式
const ICW4_8086: u8 = 0x01;
/// OCW2：非特定 EOI。原版 `BUILD_IRQ` 宏里那句 `outb 0x20,0x20`。
const EOI: u8 = 0x20;

/// 中断屏蔽字缓存。对应原版的 `cache_21` / `cache_A1`，
/// 初值 0xFF（全屏蔽）也照抄原版。
static mut CACHE_21: u8 = 0xFF;
static mut CACHE_A1: u8 = 0xFF;

/// 中断嵌套深度。对应原版全局 `intr_count`，`ret_from_sys_call` 靠它
/// 判断「现在能不能调度」。**`boot/entry.S` 直接引用这个符号**，
/// 所以必须 `no_mangle` 且是 u64。
#[unsafe(no_mangle)]
pub static mut intr_count: u64 = 0;

/// 待处理的软中断位图。对应原版 `bh_active`，同样被 entry.S 引用。
#[unsafe(no_mangle)]
pub static mut bh_active: u64 = 0;

/// 软中断使能位图。对应原版 `bh_mask`，初值全 1。
#[unsafe(no_mangle)]
pub static mut bh_mask: u64 = u64::MAX;

/// IRQ 处理函数签名。原版是 `void (*handler)(int)`，参数在 `do_IRQ` 路径下
/// 实际传的是 `(int) regs`（一个指针被塞进 int，1.0.9 的经典脏活），
/// 在 `do_fast_IRQ` 路径下传的是 irq 号。我们把两者拆开显式传：
/// `(irq, regs)`，避免那个类型双关。
pub type IrqHandler = fn(irq: usize, regs: &mut PtRegs);

/// 一条 IRQ 线的注册信息。对应原版复用的 `struct sigaction`
/// （`sa_handler` / `sa_mask` / `sa_flags`）。原版复用 sigaction 是为了省结构体，
/// 注释里也承认「it's not a 1:1 relation」；这里用专门的结构。
#[derive(Clone, Copy)]
struct IrqAction {
    handler: Option<IrqHandler>,
    /// 原版 `sa_mask`：非 0 表示该线已被占用
    in_use: bool,
    /// 原版 `SA_INTERRUPT`：处理期间保持关中断
    fast: bool,
    /// 触发次数。对应原版 `kstat.interrupts[irq]++`
    count: u64,
}

impl IrqAction {
    const fn new() -> Self {
        IrqAction { handler: None, in_use: false, fast: false, count: 0 }
    }
}

static mut IRQ_ACTION: [IrqAction; NR_IRQS] = [IrqAction::new(); NR_IRQS];

/// 未注册 IRQ 的触发计数（原版对应 `bad_IRQn_interrupt` 那组桩）。
static mut SPURIOUS_COUNT: u64 = 0;

unsafe extern "C" {
    // entry.S 里 BUILD_IRQ 宏生成的 16 个入口
    fn IRQ0_interrupt();
    fn IRQ1_interrupt();
    fn IRQ2_interrupt();
    fn IRQ3_interrupt();
    fn IRQ4_interrupt();
    fn IRQ5_interrupt();
    fn IRQ6_interrupt();
    fn IRQ7_interrupt();
    fn IRQ8_interrupt();
    fn IRQ9_interrupt();
    fn IRQ10_interrupt();
    fn IRQ11_interrupt();
    fn IRQ12_interrupt();
    fn IRQ13_interrupt();
    fn IRQ14_interrupt();
    fn IRQ15_interrupt();
}

/// 16 个入口的地址表。对应原版那三张 `interrupt[]` / `fast_interrupt[]` /
/// `bad_interrupt[]`，我们只有一张——见 entry.S 里 BUILD_IRQ 的注释。
fn irq_stub(irq: usize) -> u64 {
    // SAFETY: 只取函数地址，不调用。
    let stubs: [unsafe extern "C" fn(); NR_IRQS] = [
        IRQ0_interrupt, IRQ1_interrupt, IRQ2_interrupt, IRQ3_interrupt,
        IRQ4_interrupt, IRQ5_interrupt, IRQ6_interrupt, IRQ7_interrupt,
        IRQ8_interrupt, IRQ9_interrupt, IRQ10_interrupt, IRQ11_interrupt,
        IRQ12_interrupt, IRQ13_interrupt, IRQ14_interrupt, IRQ15_interrupt,
    ];
    stubs[irq] as u64
}

// ---- 端口读写 ----

/// # Safety
/// 必须在 CPL=0，且该端口写入对设备状态安全。
unsafe fn outb(port: u16, val: u8) {
    // SAFETY: 契约保证内核态；out 不访问内存。
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val,
                         options(nomem, nostack, preserves_flags))
    }
}

/// 从端口读一个字节。目前 PIC 的初始化不需要读回，但 IRQ 调试
/// （读 ISR/IRR）会用到，保留。
///
/// # Safety
/// 同 [`outb`]。
#[allow(dead_code)]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    // SAFETY: 契约保证内核态；in 不访问内存。
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port,
                         options(nomem, nostack, preserves_flags))
    }
    val
}

/// 带延迟的 outb。8259A 在两次写之间需要几个总线周期，
/// 原版用 `outb_p`（p = pause），实现是往 0x80 写一个字节。
///
/// # Safety
/// 同 [`outb`]。
unsafe fn outb_p(port: u16, val: u8) {
    // SAFETY: 契约转交；0x80 是 BIOS POST 诊断端口，写它没有副作用。
    unsafe {
        outb(port, val);
        outb(0x80, 0);
    }
}

// ---- 关/开中断（原版 asm/system.h 的 cli/sti/save_flags/restore_flags）----

/// 关中断并返回原 rflags。对应原版 `save_flags(flags); cli();` 这个组合。
///
/// # Safety
/// 返回值必须传给 [`restore_flags`]，否则中断状态会永久改变。
pub unsafe fn local_irq_save() -> u64 {
    let flags: u64;
    // SAFETY: pushfq/cli 在 CPL=0 合法。用 preserves_flags 是不对的
    // （cli 改 IF），所以这里不加该选项。
    unsafe {
        core::arch::asm!("pushfq", "pop {}", "cli", out(reg) flags, options(nomem));
    }
    flags
}

/// 恢复 [`local_irq_save`] 保存的中断状态。对应原版 `restore_flags(flags)`。
///
/// # Safety
/// `flags` 必须来自同一执行路径上的 [`local_irq_save`]。
pub unsafe fn restore_flags(flags: u64) {
    // SAFETY: popfq 在 CPL=0 合法；flags 来自本路径先前的 pushfq。
    unsafe {
        core::arch::asm!("push {}", "popfq", in(reg) flags, options(nomem));
    }
}

/// 开中断。对应原版 `sti()`。
///
/// # Safety
/// 调用前必须保证 IDT 已装好、当前不在不可重入的临界区里。
pub unsafe fn sti() {
    // SAFETY: 契约保证 IDT 就绪；sti 在 CPL=0 合法。
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) }
}

/// 关中断。对应原版 `cli()`。
///
/// # Safety
/// 调用方负责最终重新开中断。
pub unsafe fn cli() {
    // SAFETY: cli 在 CPL=0 合法。
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) }
}

/// 当前是否开着中断。
pub fn irqs_enabled() -> bool {
    let flags: u64;
    // SAFETY: pushfq 只读标志寄存器。
    unsafe { core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(nomem)) }
    flags & 0x200 != 0
}

// ---- 屏蔽字操作（原版 disable_irq / enable_irq）----

/// 屏蔽一条 IRQ 线。对应原版 `disable_irq()`，包括那个
/// 「主片管 0-7、从片管 8-15」的分支和屏蔽字缓存。
pub fn disable_irq(irq: usize) {
    if irq >= NR_IRQS {
        return;
    }
    let mask = 1u8 << (irq & 7);
    // SAFETY: 全程关中断，独占屏蔽字缓存与 PIC 端口。
    unsafe {
        let flags = local_irq_save();
        if irq < 8 {
            let p = core::ptr::addr_of_mut!(CACHE_21);
            *p |= mask;
            outb(PIC1_DATA, *p);
        } else {
            let p = core::ptr::addr_of_mut!(CACHE_A1);
            *p |= mask;
            outb(PIC2_DATA, *p);
        }
        restore_flags(flags);
    }
}

/// 放开一条 IRQ 线。对应原版 `enable_irq()`。
/// 从片上的线还要顺带放开主片的 IRQ2（级联线），同原版 `irqaction` 的做法。
pub fn enable_irq(irq: usize) {
    if irq >= NR_IRQS {
        return;
    }
    let mask = !(1u8 << (irq & 7));
    // SAFETY: 全程关中断，独占屏蔽字缓存与 PIC 端口。
    unsafe {
        let flags = local_irq_save();
        if irq < 8 {
            let p = core::ptr::addr_of_mut!(CACHE_21);
            *p &= mask;
            outb(PIC1_DATA, *p);
        } else {
            // 从片的中断要经 IRQ2 上报，主片那条线也必须开
            let p1 = core::ptr::addr_of_mut!(CACHE_21);
            let p2 = core::ptr::addr_of_mut!(CACHE_A1);
            *p1 &= !(1 << 2);
            *p2 &= mask;
            outb(PIC1_DATA, *p1);
            outb(PIC2_DATA, *p2);
        }
        restore_flags(flags);
    }
}

/// 当前的 16 位屏蔽字（主片低 8 位，从片高 8 位），调试用。
pub fn irq_mask() -> u16 {
    // SAFETY: 只读两个 u8 缓存。
    unsafe {
        let lo = *core::ptr::addr_of!(CACHE_21) as u16;
        let hi = *core::ptr::addr_of!(CACHE_A1) as u16;
        lo | (hi << 8)
    }
}

// ---- 注册与注销（原版 irqaction / request_irq / free_irq）----

/// 注册一个 IRQ 处理函数。对应原版 `request_irq()` → `irqaction()`。
///
/// `fast` 对应原版的 `SA_INTERRUPT` 标志：置位时处理期间保持关中断。
///
/// 返回 `Err(EINVAL)`（线号越界）或 `Err(EBUSY)`（已被占用），同原版。
pub fn request_irq(irq: usize, handler: IrqHandler, fast: bool) -> KResult<()> {
    if irq >= NR_IRQS {
        return Err(EINVAL);
    }
    // SAFETY: 全程关中断，独占 IRQ_ACTION 与 IDT 项。
    unsafe {
        let flags = local_irq_save();
        let action = &mut (*core::ptr::addr_of_mut!(IRQ_ACTION))[irq];
        if action.in_use {
            restore_flags(flags);
            return Err(EBUSY);
        }
        action.handler = Some(handler);
        action.in_use = true;
        action.fast = fast;
        // 原版在这里按 SA_INTERRUPT 选 fast_interrupt[] 或 interrupt[]；
        // 我们只有一张桩表，fast 的区别在 do_irq 里体现。
        crate::desc::set_intr_gate(IRQ_BASE + irq, irq_stub(irq));
        restore_flags(flags);
    }
    enable_irq(irq);
    Ok(())
}

/// 注销一个 IRQ 处理函数。对应原版 `free_irq()`。
pub fn free_irq(irq: usize) {
    if irq >= NR_IRQS {
        return;
    }
    disable_irq(irq);
    // SAFETY: 全程关中断，独占 IRQ_ACTION。
    unsafe {
        let flags = local_irq_save();
        let action = &mut (*core::ptr::addr_of_mut!(IRQ_ACTION))[irq];
        action.handler = None;
        action.in_use = false;
        action.fast = false;
        restore_flags(flags);
    }
}

/// 一条线至今收到多少次中断。对应原版 `kstat.interrupts[irq]`。
pub fn irq_count(irq: usize) -> u64 {
    if irq >= NR_IRQS {
        return 0;
    }
    // SAFETY: 只读一个 u64。
    unsafe { (*core::ptr::addr_of!(IRQ_ACTION))[irq].count }
}

/// 未注册线的触发次数（原版对应 `bad_IRQn_interrupt`）。
pub fn spurious_count() -> u64 {
    // SAFETY: 只读一个 u64。
    unsafe { *core::ptr::addr_of!(SPURIOUS_COUNT) }
}

// ---- 中断分发（原版 do_IRQ / do_fast_IRQ）----

/// IRQ 的统一入口，由 `boot/entry.S` 的 `irq_common` 调用。
///
/// 对应原版 `do_IRQ()` 与 `do_fast_IRQ()` 的合并：原版靠两套汇编桩区分，
/// 我们靠 `action.fast` 在这里区分（差别只是要不要 `sti`）。
///
/// EOI 的时机沿用原版 `BUILD_IRQ` 宏：**先发 EOI 再调处理函数**，
/// 这样处理函数里开中断也不会丢掉同一条线的后续中断。
///
/// # Safety
/// 只能由 entry.S 的 IRQ 桩调用，`regs` 必须指向内核栈上有效的 `PtRegs`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn do_IRQ(irq: u64, regs: *mut PtRegs) {
    let irq = irq as usize;
    // SAFETY: 契约保证 regs 有效且我们独占。
    let regs = unsafe { &mut *regs };

    // SAFETY: 单核；intr_count 只在中断进出时改，不会与自身并发。
    unsafe { *core::ptr::addr_of_mut!(intr_count) += 1 }

    // 先发 EOI（同原版）。从片的中断要给两片都发。
    // SAFETY: CPL=0；EOI 是 PIC 的标准命令。
    unsafe {
        if irq >= 8 {
            outb(PIC2_CMD, EOI);
        }
        outb(PIC1_CMD, EOI);
    }

    if irq < NR_IRQS {
        // SAFETY: irq 已边界检查；单核下同一条线不会重入（EOI 后同线中断
        // 要等 IF 打开才会再来，而下面的 handler 调用期间我们只在 !fast 时开）。
        let action = unsafe { &mut (*core::ptr::addr_of_mut!(IRQ_ACTION))[irq] };
        action.count += 1;
        match action.handler {
            Some(h) => {
                if action.fast {
                    // 原版 do_fast_IRQ：全程关中断
                    h(irq, regs);
                } else {
                    // 原版 do_IRQ：开中断跑，允许更高优先级的中断插入
                    // SAFETY: IDT 已就绪；EOI 已发，重入同一条线是安全的
                    // （handler 需自己可重入，原版对 do_IRQ 的要求相同）。
                    unsafe { sti() };
                    h(irq, regs);
                    // SAFETY: 返回 entry.S 前必须关回去，ret_from_sys_call
                    // 假设自己运行在关中断状态。
                    unsafe { cli() };
                }
            }
            None => {
                // SAFETY: 只加一个计数器。
                unsafe { *core::ptr::addr_of_mut!(SPURIOUS_COUNT) += 1 }
            }
        }
    }

    // SAFETY: 与上面的自增配对。
    unsafe { *core::ptr::addr_of_mut!(intr_count) -= 1 }
}

// ---- 软中断（原版 do_bottom_half）----

/// 软中断处理函数表。对应原版 `struct bh_struct bh_base[32]`。
static mut BH_BASE: [Option<fn()>; 32] = [None; 32];

/// 注册一个软中断。对应原版直接给 `bh_base[n].routine` 赋值
/// （如 `sched_init` 里的 `bh_base[TIMER_BH].routine = timer_bh`）。
pub fn init_bh(nr: usize, routine: fn()) {
    if nr >= 32 {
        return;
    }
    // SAFETY: 启动期单线程；只写一个函数指针。
    unsafe { (*core::ptr::addr_of_mut!(BH_BASE))[nr] = Some(routine) }
}

/// 标记一个软中断待处理。对应原版 `mark_bh(nr)`（`bh_active |= 1 << nr`）。
pub fn mark_bh(nr: usize) {
    if nr >= 32 {
        return;
    }
    // SAFETY: 单核下的读改写；中断上下文也会调它，但我们是单核且
    // 这一步在中断里是原子的（没有指令边界上的抢占）。
    unsafe { *core::ptr::addr_of_mut!(bh_active) |= 1 << nr }
}

/// 跑所有待处理的软中断。由 `boot/entry.S` 的 `handle_bottom_half` 调用。
///
/// 对应原版 `do_bottom_half()`。原版进来时已经 `sti` 且 `intr_count` 加过一，
/// 我们的 entry.S 保持同样的前置条件。
///
/// # Safety
/// 只能由 entry.S 在「已 sti、intr_count 已加一」的状态下调用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn do_bottom_half() {
    // SAFETY: 契约保证调用环境；读位图并逐位清理。
    unsafe {
        let active_p = core::ptr::addr_of_mut!(bh_active);
        let mask_p = core::ptr::addr_of!(bh_mask);
        let mut active = *active_p & *mask_p;
        let mut nr = 0usize;
        while active != 0 && nr < 32 {
            let bit = 1u64 << nr;
            if active & bit != 0 {
                // 先清标记再跑，这样处理函数里重新 mark_bh 不会丢
                *active_p &= !bit;
                active &= !bit;
                match (*core::ptr::addr_of!(BH_BASE))[nr] {
                    Some(f) => f(),
                    None => crate::pr!(Level::Err, "irq.rs: bad bottom half entry {}", nr),
                }
            }
            nr += 1;
        }
    }
}

// ---- 初始化（原版 init_IRQ + PIC 重映射）----

/// 初始化 8259A 并装好 16 个 IRQ 门。
/// 对应原版 `kernel/irq.c:init_IRQ()`，外加 PIC 重映射（见模块文档）。
///
/// # Safety
/// 必须在 [`crate::desc::init_idt`] 之后、中断关闭时调用一次。
pub unsafe fn init() {
    // SAFETY: 启动期关中断，独占 PIC 与 IDT。
    unsafe {
        // 1. 重映射：主片 → 0x20-0x27，从片 → 0x28-0x2F。
        //    标准的 ICW1..ICW4 四步序列，每步之间要留总线时间，故用 outb_p。
        outb_p(PIC1_CMD, ICW1_INIT);
        outb_p(PIC2_CMD, ICW1_INIT);
        outb_p(PIC1_DATA, IRQ_BASE as u8); // ICW2: 主片向量基址
        outb_p(PIC2_DATA, (IRQ_BASE + 8) as u8); // ICW2: 从片向量基址
        outb_p(PIC1_DATA, 1 << 2); // ICW3: 从片接在 IRQ2 上
        outb_p(PIC2_DATA, 2); // ICW3: 从片的级联身份
        outb_p(PIC1_DATA, ICW4_8086);
        outb_p(PIC2_DATA, ICW4_8086);

        // 2. 全部屏蔽，等 request_irq 逐条放开（原版 cache_* 初值就是 0xFF）
        *core::ptr::addr_of_mut!(CACHE_21) = 0xFF;
        *core::ptr::addr_of_mut!(CACHE_A1) = 0xFF;
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);

        // 3. 16 个门先全指向桩（原版 `set_intr_gate(0x20+i, bad_interrupt[i])`）。
        //    此时 IRQ_ACTION 全空，桩进来会走 do_irq 的 None 分支计入 spurious。
        for irq in 0..NR_IRQS {
            crate::desc::set_intr_gate(IRQ_BASE + irq, irq_stub(irq));
        }

        // 4. IRQ2 是级联线，永远不该有真正的处理函数，但要注册占位
        //    防止被误用。对应原版 `irqaction(2, &ignore_IRQ)`。
        let action = &mut (*core::ptr::addr_of_mut!(IRQ_ACTION))[2];
        action.handler = Some(no_action);
        action.in_use = true;
        action.fast = true;

        *core::ptr::addr_of_mut!(intr_count) = 0;
        *core::ptr::addr_of_mut!(bh_active) = 0;
    }
}

/// 什么都不做的处理函数。对应原版 `static void no_action(int cpl) { }`。
fn no_action(_irq: usize, _regs: &mut PtRegs) {}

/// 打印中断统计摘要，供启动自检。原版的等价物是 `/proc/interrupts`。
pub fn dump() {
    crate::pr!(Level::Info, "irq: mask={:#06x} intr_count={} spurious={}",
               irq_mask(), unsafe { *core::ptr::addr_of!(intr_count) }, spurious_count());
    for irq in 0..NR_IRQS {
        let c = irq_count(irq);
        if c > 0 {
            crate::pr!(Level::Info, "  irq {:2}: {} ticks", irq, c);
        }
    }
}
