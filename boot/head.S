/*
 * boot/head.S
 *
 * 参照 linux-1.0.9 的 linux/boot/head.S 重写为 64 位。
 *
 * setup 已经完成 long mode 切换，跳到本文件所在的物理地址 0x10000。
 * 这里做原版 head.S 做的那几件事（在 64 位下的等价物）：
 *   1. 重新加载段寄存器（原版 movl $0x10,%eax; mov %ax,%ds...）
 *   2. 设置内核栈（原版 lss _stack_start,%esp）
 *   3. 装载正式的 GDT 和 IDT，把 256 个门都指向 ignore_int
 *   4. 清 BSS，然后 call start_kernel
 * 原版的 setup_paging / 486 检测在 setup.S 里已完成或不再需要。
 */
    .code64
    .section .text.head
    .global _head_start
    .extern start_kernel
    .extern __bss_start
    .extern __bss_end

_head_start:
    /* 段寄存器：long mode 下只有 ds/es/ss 的选择子需要有效 */
    movw    $0x10, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movw    %ax, %fs
    movw    %ax, %gs

    /* 换到我们自己的 GDT，不再依赖 setup 留下的临时表 */
    lgdt    gdt_descr(%rip)

    /* 内核栈（对应原版 stack_start，向下增长）*/
    leaq    stack_top(%rip), %rsp
    xorq    %rbp, %rbp

    /*
     * 清 BSS 之前先把 setup.S 写在 0x90000 的机器参数保存到寄存器，
     * 因为 _kernel_end 可能超过 0x90000（由 kernel.ld ASSERT 控制上限）。
     * BootParams 只有 8 字节，E820 条目数在 0x901E0。
     */
    movq    0x90000, %r12        /* cursor(2) + ext_mem_k(2) + video_page(2) + video_mode(2) */
    movl    0x901E0, %r13d       /* e820 条目数 */

    /* 清 BSS —— Rust 侧的 static mut 和 idt 表都依赖这一步 */
    leaq    __bss_start(%rip), %rdi
    leaq    __bss_end(%rip), %rcx
    subq    %rdi, %rcx
    xorb    %al, %al
    cld
    rep     stosb

    /* 恢复 setup.S 留在 0x90000 的机器参数 */
    movq    %r12, 0x90000
    movl    %r13d, 0x901E0

    /*
     * 早期兜底 IDT：256 个门全指向 ignore_int。
     * Rust 侧的 desc::init_idt() 会在 start_kernel 里重装一张真正的 IDT，
     * 但在那之前如果发生异常（比如 mm::init 出错），有这张表至少不会
     * 三重错误重启，而是静默 iretq 后继续跑到能打印的地方。
     */
    call    setup_idt
    lidt    idt_descr(%rip)

    /* 把机器参数区地址作为第一个参数传给内核 (SysV ABI: rdi) */
    movq    $0x90000, %rdi
    call    start_kernel

    /* start_kernel 不应返回；万一返回就停机（原版是 L6: jmp L6）*/
L6: cli
    hlt
    jmp     L6

/*
 * ignore_int —— 与原版同名同职责：未初始化的中断进来时打印提示后返回。
 * 这里只做最小处理：什么都不打印，直接 iretq，避免在早期启动阶段
 * 因为串口/显存未就绪而二次异常。
 */
ignore_int:
    iretq

/*
 * setup_idt —— 对应原版 setup_idt，把 256 个门全部填成 ignore_int。
 * 64 位门描述符是 16 字节，格式：
 *   [0:1]  offset低16  [2:3] selector  [4] ist  [5] type/attr
 *   [6:7]  offset16-31 [8:11] offset32-63 [12:15] reserved
 */
    .section .text
    .global setup_idt
setup_idt:
    leaq    ignore_int(%rip), %rax
    leaq    idt(%rip), %rdi
    movl    $256, %ecx
1:  movw    %ax, (%rdi)                 /* offset 低 16 位 */
    movw    $0x08, 2(%rdi)              /* 内核代码段选择子 */
    movb    $0, 4(%rdi)                 /* IST = 0 */
    movb    $0x8E, 5(%rdi)              /* present, DPL=0, 64 位中断门 */
    movq    %rax, %rdx
    shrq    $16, %rdx
    movw    %dx, 6(%rdi)
    shrq    $16, %rdx
    movl    %edx, 8(%rdi)
    movl    $0, 12(%rdi)
    addq    $16, %rdi
    decl    %ecx
    jnz     1b
    ret

/* ---- 描述符表与栈 ---- */
    .section .data
    .align 16
    .global gdt
gdt:
    .quad   0x0000000000000000          /* 0x00: NULL */
    .quad   0x00AF9A000000FFFF          /* 0x08: 内核代码 (L=1, DPL=0) */
    .quad   0x00CF92000000FFFF          /* 0x10: 内核数据 */
    .quad   0x00AFFA000000FFFF          /* 0x18: 用户代码 (DPL=3) */
    .quad   0x00CFF2000000FFFF          /* 0x20: 用户数据 (DPL=3) */
    .space  256*8 - 40                  /* 预留 GDT 空间, 同原版 256 项 */
gdt_end:

gdt_descr:
    .word   gdt_end - gdt - 1
    .quad   gdt

idt_descr:
    .word   256*16 - 1
    .quad   idt

    .section .bss
    .align 16
    .global idt
idt:
    .space  256*16

    .align 4096
    /*
     * 栈溢出护栏。task[0]（start_kernel 的执行流）用的就是这个栈，
     * 而链接器把它排在 .bss 里 Rust 那些 static mut 表（BUFFERS /
     * INODES / TASKS）的**后面**——地址更高。栈向下增长，溢出就会
     * 从高地址往低地址踩进 BUFFERS，症状是缓冲头的 b_size/b_data 变
     * 成 0（越界那一头正好落在 BUFFERS 的高下标上），或者链接下标里
     * 出现别的表的字节。这类损坏离原因很远，所以在栈底之下留一页
     * 哨兵，由 stack_guard_ok() 检查（见 src/sched/mod.rs）。
     */
    .align 4096
    .global stack_guard
stack_guard:
    .space  512                         /* 哨兵：512 字节够检出连续下溢 */
    .global stack_bottom
stack_bottom:
    /*
     * task[0] 的栈。16KB：SMP 调度器（sched_lock/switch_to_task 的
     * 现场元组 + this_cpu 调用链）把高水位从 3832 顶到 8176 字节，
     * 8KB 已经撞穿哨兵（高地址溢出会踩进 BUFFERS/静态表，故障离
     * 原因非常远）。BSS 已挪到 1MB 起步，assert 上限 2MB，放得下。
     */
    .space  16384
    .global stack_top
stack_top:
