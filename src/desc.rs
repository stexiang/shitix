//! GDT / TSS / IDT 的建立与装载。
//!
//! 对应 linux-1.0.9 的：
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`SEGMENT`] 里的选择子常量 | `include/linux/segment.h` 的 `KERNEL_CS`/`USER_DS` 等 |
//! | [`init_gdt`] | `boot/head.S` 的 `gdt` 表 + `sched.c:sched_init` 的 `set_tss_desc` |
//! | [`init_idt`] | `kernel/traps.c:trap_init()` + `asm/system.h` 的 `set_*_gate` 宏 |
//!
//! 与原版的三处结构性差异，都是 long mode 逼出来的：
//!
//! 1. **TSS 不再用于任务切换**。原版每个进程一个 TSS，`switch_to` 是一条
//!    `ljmp` 到该 TSS 的描述符，CPU 自动换全部寄存器。long mode 删了这个机制，
//!    TSS 只剩 `rsp0`（特权级切换时的内核栈）和 IST。所以我们只需要**一个**
//!    TSS，`rsp0` 在每次调度时改写；原版 `_TSS(n)` / `FIRST_TSS_ENTRY` 那套
//!    每任务一个描述符的布局不再需要。
//! 2. **段寄存器基址强制为 0**。原版靠 `USER_DS` 的段基址实现 3GB 用户空间
//!    分割（`TASK_SIZE 0xc0000000`），64 位下只能靠页表，所以数据段描述符
//!    只剩「DPL + 可写」有意义。
//! 3. **IST**：原版没有这个概念。double fault 和 NMI 走独立栈，否则栈溢出
//!    引发的 page fault 会立刻升级成三重错误——这正是我们最想避免的失败模式。

use crate::klib::printk::Level;
use core::mem::size_of;

/// 段选择子。数值必须与 `boot/head.S` 的 `gdt` 表和 `boot/entry.S` 的
/// `KERNEL_CS`/`KERNEL_DS` 常量一致（head.S 在 Rust 接管前就要用）。
///
/// 对应原版 `include/linux/segment.h`：
/// ```text
/// #define KERNEL_CS 0x10   （原版索引 2，我们是 1）
/// #define KERNEL_DS 0x18
/// #define USER_CS   0x23
/// #define USER_DS   0x2B
/// ```
/// 索引不同是因为原版 GDT 前两项被 head.S 的临时描述符占着，我们没那个包袱。
pub mod selector {
    /// 内核代码段（GDT 索引 1，RPL=0）
    pub const KERNEL_CS: u16 = 0x08;
    /// 内核数据段（GDT 索引 2）
    pub const KERNEL_DS: u16 = 0x10;
    /// 用户代码段（GDT 索引 3，RPL=3）
    pub const USER_CS: u16 = 0x18 | 3;
    /// 用户数据段（GDT 索引 4，RPL=3）
    pub const USER_DS: u16 = 0x20 | 3;
    /// TSS 描述符（GDT 索引 5，占两项因为 64 位 TSS 描述符是 16 字节）
    pub const TSS: u16 = 0x28;
}

/// IST 槽位分配。原版没有 IST（32 位不支持）。
pub mod ist {
    /// double fault 专用栈：内核栈溢出时唯一还能站住脚的地方
    pub const DOUBLE_FAULT: u16 = 1;
    /// NMI 专用栈
pub const NMI: u16 = 2;
    /// page fault 专用栈：栈溢出的第一现场通常是 page fault
    pub const PAGE_FAULT: u16 = 3;
}

/// 每个 IST 栈的大小。原版内核栈是一整页（4KB），我们给异常栈同样的量。
const EXC_STACK_SIZE: usize = 4096 * 2;

/// 三个异常专用栈。放 BSS，由 head.S 清零。
/// 字段本身从不被读——它只是占住地址空间，实际使用是 CPU 通过 IST
/// 里记下的栈顶地址去写。
#[repr(align(16))]
struct ExcStack(#[allow(dead_code)] [u8; EXC_STACK_SIZE]);

static mut DF_STACK: ExcStack = ExcStack([0; EXC_STACK_SIZE]);
static mut NMI_STACK: ExcStack = ExcStack([0; EXC_STACK_SIZE]);
static mut PF_STACK: ExcStack = ExcStack([0; EXC_STACK_SIZE]);

// ---- GDT ----

/// 64 位 GDT 项。NULL/代码/数据段是 8 字节，TSS 描述符是 16 字节（占两格）。
#[derive(Clone, Copy)]
#[repr(transparent)]
struct GdtEntry(u64);

/// GDT 本体：NULL + 内核 CS/DS + 用户 CS/DS + TSS(2 格) = 7 格。
/// 原版留了 256 项（`head.S` 的 `.fill 256-6,8,0`）给每任务的 TSS/LDT，
/// 我们不需要——见模块文档第 1 点。
const GDT_LEN: usize = 7;

static mut GDT: [GdtEntry; GDT_LEN] = [GdtEntry(0); GDT_LEN];

/// 唯一的 TSS。对应原版 `init_task.tss`，但只用 `rsp0` 和 IST 两部分。
#[repr(C, packed(4))]
struct Tss {
    reserved0: u32,
    /// rsp0/rsp1/rsp2：从低特权级陷入时用的栈。我们只用 rsp0。
    privilege_stack_table: [u64; 3],
    reserved1: u64,
    /// IST 1..7（下标 0 对应 IST1）
    interrupt_stack_table: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    /// I/O 权限位图偏移。设成 TSS 大小即「没有位图」，
    /// 等价于原版把 `tss.bitmap` 指向全 1 的 `io_bitmap`（禁止用户态 in/out）。
    iomap_base: u16,
}

static mut TSS: Tss = Tss {
    reserved0: 0,
    privilege_stack_table: [0; 3],
    reserved1: 0,
    interrupt_stack_table: [0; 7],
    reserved2: 0,
    reserved3: 0,
    iomap_base: size_of::<Tss>() as u16,
};

/// `lgdt` / `lidt` 的操作数格式：2 字节 limit + 8 字节基址。
#[derive(Clone, Copy)]
#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// 建立并装载 GDT + TSS。
///
/// 对应原版两处的合并：`boot/head.S` 里静态写死的 `gdt` 表，
/// 以及 `sched.c:sched_init()` 里的 `set_tss_desc(gdt+FIRST_TSS_ENTRY, &init_task.tss)`
/// + `load_TR(0)`。
///
/// # Safety
/// 启动早期、中断关闭时调用一次。调用后 head.S 那张临时 GDT 即失效，
/// 但因为选择子数值和段属性都兼容，正在执行的代码不受影响。
pub unsafe fn init_gdt() {
    // 段描述符的标志位。long mode 下代码段只有 DPL / L / P 有意义，
    // 数据段只有 DPL / W / P 有意义，limit 和 base 全被忽略。
    const ACCESSED: u64 = 1 << 40;
    const WRITABLE: u64 = 1 << 41;
    const EXECUTABLE: u64 = 1 << 43;
    const USER_SEGMENT: u64 = 1 << 44;
    const PRESENT: u64 = 1 << 47;
    /// bit 53 = L：64 位代码段
    const LONG_MODE: u64 = 1 << 53;
    /// bit 54 = D/B：32 位操作数/栈。**代码段绝不能与 L 同时置位**
    /// （L=1 且 D=1 是保留组合，加载这样的 CS 会 #GP）。见 bug-003。
    const DEFAULT_SIZE: u64 = 1 << 54;
    /// bit 55 = G：limit 以 4KB 为单位
    const GRANULARITY: u64 = 1 << 55;
    const DPL_RING3: u64 = 3 << 45;
    // 即使 long mode 下 limit 被忽略，也照原版填满，方便用调试器核对
    const LIMIT_FLAT: u64 = 0xF_0000_0000_FFFF;

    let common = ACCESSED | USER_SEGMENT | PRESENT | LIMIT_FLAT | GRANULARITY;
    // 与 head.S 的 0x00AF9A000000FFFF 逐位一致（0xAF: G=1 D=0 L=1）
    let kernel_code = common | EXECUTABLE | WRITABLE | LONG_MODE;
    // 与 head.S 的 0x00CF92000000FFFF 逐位一致（0xCF: G=1 D=1 L=0）
    let kernel_data = common | WRITABLE | DEFAULT_SIZE;
    let user_code = kernel_code | DPL_RING3;
    let user_data = kernel_data | DPL_RING3;

    // SAFETY: 启动早期单线程独占；addr_of_mut 避免直接借用 static mut。
    let gdt = unsafe { &mut *core::ptr::addr_of_mut!(GDT) };
    gdt[0] = GdtEntry(0);
    gdt[1] = GdtEntry(kernel_code);
    gdt[2] = GdtEntry(kernel_data);
    gdt[3] = GdtEntry(user_code);
    gdt[4] = GdtEntry(user_data);

    // TSS 描述符（16 字节，占 gdt[5] 和 gdt[6]）。
    // 对应原版 `set_tss_desc` 那个 `_set_tssldt_desc` 汇编宏，
    // 只是 64 位下基址扩到 64 位，所以要跨两格。
    let tss_addr = core::ptr::addr_of!(TSS) as u64;
    let limit = (size_of::<Tss>() - 1) as u64;
    // type=0x9 (available 64-bit TSS), P=1, DPL=0
    let low = limit & 0xFFFF
        | (tss_addr & 0xFF_FFFF) << 16
        | 0x9 << 40
        | PRESENT
        | ((limit >> 16) & 0xF) << 48
        | ((tss_addr >> 24) & 0xFF) << 56;
    let high = tss_addr >> 32;
    gdt[5] = GdtEntry(low);
    gdt[6] = GdtEntry(high);

    // 三个 IST 栈的栈顶（向下增长，所以是数组末尾）
    // SAFETY: 三个静态数组独占，取末尾地址不解引用。
    unsafe {
        let tss = &mut *core::ptr::addr_of_mut!(TSS);
        tss.interrupt_stack_table[(ist::DOUBLE_FAULT - 1) as usize] =
            core::ptr::addr_of!(DF_STACK) as u64 + EXC_STACK_SIZE as u64;
        tss.interrupt_stack_table[(ist::NMI - 1) as usize] =
            core::ptr::addr_of!(NMI_STACK) as u64 + EXC_STACK_SIZE as u64;
        tss.interrupt_stack_table[(ist::PAGE_FAULT - 1) as usize] =
            core::ptr::addr_of!(PF_STACK) as u64 + EXC_STACK_SIZE as u64;
    }

    let ptr = DescriptorTablePointer {
        limit: (size_of::<[GdtEntry; GDT_LEN]>() - 1) as u16,
        base: core::ptr::addr_of!(GDT) as u64,
    };

    // SAFETY: ptr 描述的是刚填好的合法 GDT；新表的选择子数值与 head.S 的旧表
    // 兼容（CS=0x08 仍是 64 位内核代码段，DS=0x10 仍是内核数据段），
    // 所以 lgdt 之后无需重载段寄存器即可继续执行。ltr 装载的 TSS 描述符
    // 刚在 gdt[5..7] 建好且 busy 位为 0。
    unsafe {
        core::arch::asm!(
            "lgdt [{ptr}]",
            "ltr {tss:x}",
            ptr = in(reg) &ptr,
            tss = in(reg) selector::TSS,
            options(readonly, nostack, preserves_flags)
        );
    }
}

/// 仅供 AP（应用处理器）启动时调用：装载与 BSP 相同的 GDT 和 IDT，
/// 但 **不** `ltr` —— TSS 由 BSP 独占（AP 不跑用户任务，没有特权级
/// 切换要用 rsp0），且 TSS 描述符已被 BSP 的 `ltr` 标成 busy，AP 再
/// `ltr` 同一张会 #GP。
///
/// CS 用 retfq 重载到 `KERNEL_CS`：AP 醒来时 CS 是蹦床 GDT 的 0x18，
/// 恰好等于本表的用户代码段索引，虽然 flat 段下继续执行不会出错，
/// 但任何远转移都会拿错段，必须换掉。
///
/// # Safety
/// 只在 AP 蹦床把执行权交给 Rust 后调用一次。调用者必须保证
/// [`init_gdt`]/[`init_idt`] 已在 BSP 上完成（表已建好）。
pub unsafe fn ap_load_tables() {
    let gdt_ptr = DescriptorTablePointer {
        limit: (size_of::<[GdtEntry; GDT_LEN]>() - 1) as u16,
        base: core::ptr::addr_of!(GDT) as u64,
    };
    let idt_ptr = DescriptorTablePointer {
        limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: core::ptr::addr_of!(IDT) as u64,
    };
    // SAFETY: 两张表都由 BSP 建好后不再改动；lretq 用的新 CS 是合法的
    // 64 位内核代码段。栈操作是指令语义的一部分，不能声明 nostack。
    unsafe {
        core::arch::asm!(
            "lgdt ({gdt})",
            "lidt ({idt})",
            "pushq {cs}",
            "leaq 2f(%rip), %rax",
            "pushq %rax",
            "lretq",
            "2:",
            gdt = in(reg) &gdt_ptr,
            idt = in(reg) &idt_ptr,
            cs = in(reg) selector::KERNEL_CS as u64,
            out("rax") _,
            options(att_syntax, preserves_flags)
        );
    }
}

/// 改写 TSS.rsp0 —— 每次调度切到新任务时调用，让下一次从用户态陷入时
/// 落到该任务自己的内核栈上。
///
/// 对应原版 `p->tss.esp0 = p->kernel_stack_page + PAGE_SIZE`，只是原版每个
/// 任务有独立 TSS 所以只在 fork 时写一次；我们共用一个 TSS，必须每次切换都写。
///
/// # Safety
/// `rsp0` 必须是某个存活任务内核栈的栈顶，且 16 字节对齐。
pub unsafe fn set_rsp0(rsp0: u64) {
    // SAFETY: 单核无抢占；只改一个 u64 字段，CPU 只在下一次特权级切换时读它。
    unsafe { (*core::ptr::addr_of_mut!(TSS)).privilege_stack_table[0] = rsp0 }
}

/// 当前 TSS.rsp0，调试用。
pub fn rsp0() -> u64 {
    // SAFETY: 只读一个 u64。
    unsafe { (*core::ptr::addr_of!(TSS)).privilege_stack_table[0] }
}

/// 打印 GDT/TSS 摘要，供启动自检。
pub fn dump() {
    // SAFETY: 只读静态表的地址与已填好的内容。
    let (gdt_base, tss_base) =
        (core::ptr::addr_of!(GDT) as u64, core::ptr::addr_of!(TSS) as u64);
    crate::pr!(Level::Info, "gdt: base={:#x} entries={} tss={:#x} rsp0={:#x}",
               gdt_base, GDT_LEN, tss_base, rsp0());
}

// ---- IDT ----

/// 64 位门描述符（16 字节）。
///
/// 对应原版 `asm/system.h` 的 `_set_gate` 宏操作的那个 8 字节结构。
/// 64 位下多了 IST 字段、偏移扩到 64 位。
#[derive(Clone, Copy)]
#[repr(C)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    /// 低 3 位是 IST 索引，其余保留
    ist: u8,
    /// P | DPL | 0 | type：0xE=中断门(自动 cli)，0xF=陷阱门(不 cli)
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const fn empty() -> Self {
        IdtEntry {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    /// 填一个门。`kind` 取 [`GateKind`]，`dpl` 是允许从哪个特权级用 `int n` 触发。
    fn set(&mut self, handler: u64, kind: GateKind, dpl: u8, ist_index: u16) {
        self.offset_low = handler as u16;
        self.selector = selector::KERNEL_CS;
        self.ist = (ist_index & 0x7) as u8;
        self.type_attr = 0x80 | ((dpl & 3) << 5) | kind as u8;
        self.offset_mid = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.reserved = 0;
    }
}

/// 门类型。对应原版 `set_intr_gate`(14) / `set_trap_gate`(15) 里那个 type 参数。
#[derive(Clone, Copy)]
#[repr(u8)]
enum GateKind {
    /// 中断门：进入时 CPU 自动清 IF。原版 `set_intr_gate` 用它装 IRQ。
    Interrupt = 0xE,
    /// 陷阱门：进入时不动 IF。原版 `set_trap_gate` 用它装异常。
    Trap = 0xF,
}

/// IDT 本体，256 个门。原版是 `head.S` 里的 `_idt`，
/// 由 `trap_init()` / `init_IRQ()` 分两批填。
static mut IDT: [IdtEntry; 256] = [IdtEntry::empty(); 256];

unsafe extern "C" {
    // 异常桩，全部来自 boot/entry.S（原版对应 sys_call.S 的同名符号）
    fn divide_error();
    fn debug();
    fn nmi();
    fn int3();
    fn overflow();
    fn bounds();
    fn invalid_op();
    fn device_not_available();
    fn double_fault();
    fn coprocessor_segment_overrun();
    fn invalid_TSS();
    fn segment_not_present();
    fn stack_segment();
    fn general_protection();
    fn page_fault();
    fn reserved();
    fn coprocessor_error();
    fn alignment_check();
    fn machine_check();
    fn simd_coprocessor_error();
    fn virtualization();
    /// int 0x80 入口（原版 `_system_call`）
    fn system_call();
}

/// 装一个中断门（DPL=0，自动 cli）。对应原版 `set_intr_gate(n, addr)`。
///
/// # Safety
/// `handler` 必须是一个符合 entry.S 里 pt_regs 约定的裸函数地址。
pub unsafe fn set_intr_gate(n: usize, handler: u64) {
    // SAFETY: n < 256 由下面的断言保证；单核启动期独占 IDT。
    unsafe { (*core::ptr::addr_of_mut!(IDT))[n].set(handler, GateKind::Interrupt, 0, 0) }
}

/// 装一个陷阱门（DPL=0，不动 IF）。对应原版 `set_trap_gate(n, addr)`。
///
/// # Safety
/// 同 [`set_intr_gate`]。
pub unsafe fn set_trap_gate(n: usize, handler: u64) {
    // SAFETY: 同上。
    unsafe { (*core::ptr::addr_of_mut!(IDT))[n].set(handler, GateKind::Trap, 0, 0) }
}

/// 装一个用户可触发的陷阱门（DPL=3）。对应原版 `set_system_gate(n, addr)`，
/// 原版用它装 int3/int4/int5 和 0x80。
///
/// # Safety
/// 同 [`set_intr_gate`]。注意 DPL=3 意味着用户态能用 `int n` 主动触发。
pub unsafe fn set_system_gate(n: usize, handler: u64) {
    // SAFETY: 同上。
    unsafe { (*core::ptr::addr_of_mut!(IDT))[n].set(handler, GateKind::Trap, 3, 0) }
}

/// 装一个走 IST 独立栈的陷阱门。原版没有对应物（32 位无 IST）。
///
/// # Safety
/// 同 [`set_intr_gate`]，且 `ist_index` 必须是 [`ist`] 里已分配好栈的槽位。
pub unsafe fn set_trap_gate_ist(n: usize, handler: u64, ist_index: u16) {
    // SAFETY: 同上；ist_index 的有效性由调用方保证。
    unsafe { (*core::ptr::addr_of_mut!(IDT))[n].set(handler, GateKind::Trap, 0, ist_index) }
}

/// 建立并装载 IDT。对应原版 `kernel/traps.c` 的 `trap_init()`。
///
/// # Safety
/// 必须在 [`init_gdt`] 之后、中断关闭时调用一次。调用后 head.S 那张
/// 全指向 `ignore_int` 的临时 IDT 即被取代。
pub unsafe fn init_idt() {
    // SAFETY: 下面每个符号都是 entry.S 里按 pt_regs 约定写的裸入口。
    // 顺序与原版 trap_init() 逐行对应。
    unsafe {
        set_trap_gate(0, divide_error as *const () as u64);
        set_trap_gate(1, debug as *const () as u64);
        // NMI 走独立栈：它可能在任何时刻打断内核，包括栈将要溢出时
        set_trap_gate_ist(2, nmi as *const () as u64, ist::NMI);
        // int3/4/5 原版是 set_system_gate（"can be called from all"）
        set_system_gate(3, int3 as *const () as u64);
        set_system_gate(4, overflow as *const () as u64);
        set_system_gate(5, bounds as *const () as u64);
        set_trap_gate(6, invalid_op as *const () as u64);
        set_trap_gate(7, device_not_available as *const () as u64);
        // double fault 必须走 IST：走到这里说明处理上一个异常时又出错了，
        // 当前栈已不可信。原版在 32 位下没有这个保护，只能三重错误重启。
        set_trap_gate_ist(8, double_fault as *const () as u64, ist::DOUBLE_FAULT);
        set_trap_gate(9, coprocessor_segment_overrun as *const () as u64);
        set_trap_gate(10, invalid_TSS as *const () as u64);
        set_trap_gate(11, segment_not_present as *const () as u64);
        set_trap_gate(12, stack_segment as *const () as u64);
        set_trap_gate(13, general_protection as *const () as u64);
        // page fault 也走 IST：内核栈溢出的第一现场就是它
        set_trap_gate_ist(14, page_fault as *const () as u64, ist::PAGE_FAULT);
        set_trap_gate(15, reserved as *const () as u64);
        set_trap_gate(16, coprocessor_error as *const () as u64);
        set_trap_gate(17, alignment_check as *const () as u64);
        // 18..20 是 64 位新增的向量，原版的 `for (i=18;i<48;i++)` 把它们
        // 全填成 reserved；我们给前三个真正的处理函数，其余照原版填 reserved。
        set_trap_gate(18, machine_check as *const () as u64);
        set_trap_gate(19, simd_coprocessor_error as *const () as u64);
        set_trap_gate(20, virtualization as *const () as u64);
        for n in 21..32 {
            set_trap_gate(n, reserved as *const () as u64);
        }
        // 0x20..0x30 的 IRQ 门由 irq::init() 装（对应原版 init_IRQ()）；
        // 剩下的软中断向量填 reserved，同原版。
        for n in 0x30..256 {
            set_trap_gate(n, reserved as *const () as u64);
        }
        // int 0x80：DPL=3 让用户态能触发。对应原版 sched_init() 里那句
        // `set_system_gate(0x80, &system_call)`。
        set_system_gate(0x80, system_call as *const () as u64);
    }

    let ptr = DescriptorTablePointer {
        limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: core::ptr::addr_of!(IDT) as u64,
    };
    // SAFETY: ptr 描述刚填好的 256 项合法 IDT，每个门都指向 entry.S 里的真实入口。
    unsafe {
        core::arch::asm!("lidt [{}]", in(reg) &ptr,
                         options(readonly, nostack, preserves_flags));
    }

    // 配置 syscall 指令的 MSR
    // 配置 syscall 指令的 MSR
    init_syscall_msrs();
}

/// 写 syscall 指令所需的 MSR（STAR/LSTAR/SFMASK）。
fn init_syscall_msrs() {
    // SAFETY: CPL=0，wrmsr 合法。
    unsafe {
        // IA32_STAR (0xC0000081) 布局：
        //   bits [31:0]   保留（必须 0）          → eax
        //   bits [47:32]  syscall 内核 CS          → edx 低 16
        //   bits [63:48]  sysret 用户 CS 基址       → edx 高 16
        // 旧代码把 0x0008_0000 写进 eax、edx 写 0，
        // 结果 STAR[47:32]=0：`syscall` 把内核 CS 加载成 0（NULL）。
        // 代码在 long mode 平坦段下能跑，但 swapper 被时钟打断后 iretq
        // 试图恢复 CS=0 → #GP(0)，shell 打完提示词 "/ # " 即崩溃。
        let kernel_cs = selector::KERNEL_CS as u32;          // 0x08
        let user_cs_base = (selector::USER_CS & !3) as u32;  // 0x18
        let edx = (user_cs_base << 16) | kernel_cs;          // 0x0018_0008
        core::arch::asm!("wrmsr",
            in("ecx") 0xC000_0081u32,
            in("eax") 0u32,
            in("edx") edx,
            options(nomem, nostack, preserves_flags));

        // IA32_LSTAR: syscall_entry 地址
        unsafe extern "C" { fn syscall_entry(); }
        let lstar = syscall_entry as *const () as u64;
        core::arch::asm!("wrmsr",
            in("ecx") 0xC000_0082u32,
            in("eax") lstar as u32,
            in("edx") (lstar >> 32) as u32,
            options(nomem, nostack, preserves_flags));

        // IA32_SFMASK: 进入时自动清 IF(0x200)
        core::arch::asm!("wrmsr",
            in("ecx") 0xC000_0084u32,
            in("eax") 0x200u32,
            in("edx") 0u32,
            options(nomem, nostack, preserves_flags));
    }
}
