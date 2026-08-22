/*
 * boot/ap_trampoline.S — AP (Application Processor) 启动蹦床
 *
 * SMP 引导流程（Intel MP 规范 INIT-SIPI-SIPI）：
 *   BSP 把本文件产出的二进制拷贝到物理 0x2000（低 1MB 保留区，
 *   页分配器从 1MB 起分配，永不回收这里），填好页尾的信箱
 *   （CR3 / 栈顶 / Rust 入口 / CPU 逻辑号），然后向目标 AP 发
 *   INIT IPI + 两次 SIPI（vector = 0x2000 >> 12 = 0x02）。
 *
 *   AP 在实模式从 CS:IP = 0x0200:0x0000（即物理 0x2000）醒来，
 *   依次完成：实模式 → 保护模式 → 开 PAE/LME/分页 → long mode，
 *   最后从信箱取栈和入口地址，跳进 Rust 的 smp_ap_main()。
 *
 * 链接基址固定 0x2000（ld -Ttext 0x2000），所以文件里所有符号都是
 * 绝对物理地址：实模式段内偏移、保护模式线性地址、long mode 恒等
 * 映射虚拟地址三者在这个地址下恰好相等（低 1GB 恒等映射）。
 *
 * 为什么放 0x2000：0x0000-0x0FFF 是 IVT/BDA，0x4000-0x7FFF 是引导
 * 页表，0x8000 起已经被内核镜像（0x10000.._kernel_end）覆盖；
 * 只有 0x1000-0x3FFF 这 8KB 是谁都不用的空隙，取后 4KB。
 */

    .set TRAMP_BASE,    0x2000

    /* 信箱布局：页尾 0xF00 起，BSP 在 SIPI 之前填写 */
    .set TRAMP_MAGIC,   TRAMP_BASE + 0xF00   /* u64: 0x534D5054524D5021 ("SMPTRMP!") */
    .set TRAMP_CR3,     TRAMP_BASE + 0xF08   /* u64: 内核 PML4 物理地址 */
    .set TRAMP_STACK,   TRAMP_BASE + 0xF10   /* u64: 本 AP 的栈顶（高半区虚拟地址） */
    .set TRAMP_ENTRY,   TRAMP_BASE + 0xF18   /* u64: Rust 入口 smp_ap_main */
    .set TRAMP_CPU_ID,  TRAMP_BASE + 0xF20   /* u64: 逻辑 CPU 号（传 rdi） */

    .section .text
    .global _tramp_start

/* ------------------------------------------------------------------ */
/* 第一阶段：实模式（SIPI 入口点，必须在页偏移 0）                      */
/* ------------------------------------------------------------------ */
    .code16
_tramp_start:
    cli
    cld
    xorw    %ax, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss

    /* 装载蹦床自带 GDT。data32 让基址按完整 32 位装载（虽然只有 24 位
     * 有效，但基址 0x2xxx 本来就在 24 位内，加前缀只是保险）。 */
    data32 lgdt tramp_gdt_desc

    /* CR0.PE = 1，进保护模式 */
    movl    %cr0, %eax
    orl     $0x1, %eax
    movl    %eax, %cr0

    /* 远跳刷新 CS。目标 < 64KB，16 位偏移够用，不需要 66 前缀 */
    ljmp    $0x08, $tramp_pm32

/* ------------------------------------------------------------------ */
/* 第二阶段：32 位保护模式，开 PAE + LME + 分页                        */
/* ------------------------------------------------------------------ */
    .code32
tramp_pm32:
    movw    $0x10, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movw    %ax, %fs
    movw    %ax, %gs

    /* CR4 = PAE | PGE | OSFXSR | OSXMMEXCPT，与 setup.S 给 BSP 配的
     * 0x6A0 一致。OSFXSR 不开的话 Rust 代码里的 SSE（movaps 等）
     * 会立刻 #UD。 */
    movl    %cr4, %eax
    orl     $0x6A0, %eax
    movl    %eax, %cr4

    /* CR3 = 内核 PML4（BSP 填在信箱里）。低 1GB 恒等映射 + 高半区
     * 直接映射都在这张表里。 */
    movl    TRAMP_CR3, %eax
    movl    %eax, %cr3

    /* EFER.LME = 1 */
    movl    $0xC0000080, %ecx
    rdmsr
    orl     $(1 << 8), %eax
    wrmsr

    /* CR0：清 INIT 复位留下的 CD/NW（否则 AP 全程 uncached），
     * 置 PG | WP | PE（WP 与 BSP 的 CR0.WP=1 保持一致，内核态写
     * COW 只读页才会缺页而不是写穿）。 */
    movl    %cr0, %eax
    andl    $0x9FFFFFFF, %eax        /* ~((1<<30)|(1<<29)) */
    orl     $0x80010001, %eax        /* PG | WP | PE */
    movl    %eax, %cr0

    ljmp    $0x18, $tramp_pm64

/* ------------------------------------------------------------------ */
/* 第三阶段：64 位 long mode，取信箱参数，跳 Rust                      */
/* ------------------------------------------------------------------ */
    .code64
tramp_pm64:
    movw    $0x10, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    xorw    %ax, %ax
    movw    %ax, %fs
    movw    %ax, %gs

    /* 信箱都在低 1MB，恒等映射下直接按绝对地址读 */
    movq    TRAMP_STACK, %rsp
    movq    TRAMP_CPU_ID, %rdi       /* 第一个参数：逻辑 CPU 号 */
    movq    TRAMP_ENTRY, %rax
    /* 入口约定：fn(u64) -> !，SysV ABI，栈已 16 字节对齐（BSP 保证）。
     * 不返回，所以不需要对齐 call 的返回地址槽，直接 jmp。 */
    jmp     *%rax

/* 保险丝：若意外走到这里，停机 */
tramp_halt:
    hlt
    jmp     tramp_halt

/* ------------------------------------------------------------------ */
/* 蹦床自带 GDT：null + 32 位代码 + 数据 + 64 位代码                   */
/* ------------------------------------------------------------------ */
    .p2align 4
tramp_gdt:
    .quad   0x0000000000000000       /* 0x00 null */
    .quad   0x00CF9A000000FFFF       /* 0x08 32 位代码: base 0, limit 4G, D=1 */
    .quad   0x00CF92000000FFFF       /* 0x10 数据:     base 0, limit 4G */
    .quad   0x00AF9A000000FFFF       /* 0x18 64 位代码: L=1, D=0 */
tramp_gdt_end:
tramp_gdt_desc:
    .word   tramp_gdt_end - tramp_gdt - 1
    .long   tramp_gdt

    .global _tramp_end
_tramp_end:
