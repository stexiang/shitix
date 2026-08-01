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

    /* 清 BSS —— Rust 侧的 static mut 和 idt 表都依赖这一步 */
    leaq    __bss_start(%rip), %rdi
    leaq    __bss_end(%rip), %rcx
    subq    %rdi, %rcx
    xorb    %al, %al
    cld
    rep     stosb

    /* 填好 256 个门再 lidt（顺序不能反，否则装的是空表）*/
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
stack_bottom:
    .space  16384                       /* 16KB 内核栈 */
stack_top:
