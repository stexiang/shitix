/*
 * boot/setup.S
 *
 * 参照 linux-1.0.9 的 linux/boot/setup.S 重写并扩展到 64 位。
 *
 * 原版 setup.S 的职责：用 BIOS 收集机器参数存到 0x90000、打开 A20、
 * 屏蔽 8259、装载临时 GDT/IDT、切到保护模式并跳到 system。
 * 本文件保留同样的职责，但最后一步是切换到 **long mode (IA-32e)**：
 *   real mode -> 建立 4 级页表 -> PAE + LME + PG/PE 一次开启 -> 64 位
 *
 * 与原版不同：不再把 system 从 0x10000 搬到 0x0000（long mode 下有
 * 完整的恒等映射，不需要腾出低端 1MB），因此省掉 do_move 循环。
 */
    .code16
    .section .text
    .global _setup_start

    .set INITSEG,  0x9000       /* 机器参数区（与原版一致）*/
    .set SYSSEG,   0x1000       /* system 所在段, 物理 0x10000 */
/* 页表放在 0x4000 起的 3 个页。
 *
 * 原来在 0x70000，但内核镜像从 0x10000 装载，BSS 增长到 0x76680 之后
 * head.S 的清 BSS 会把页表清零 → 切 long mode 时立刻三重错误。
 * 0x4000-0x6FFF 这一段在 long mode 之后没人用：bootsect 自搬到 0x9000、
 * setup 在 0x90200、机器参数区 0x90000、E820 表 0x9E000，
 * 而 bootsect 的实模式栈在 0x9000:0x4000（即物理 0x94000）不在这里。
 * page_alloc 把整个低 1MB 都列为保留（见 MIN_USABLE_PHYS），
 * 所以这三页不会被当成空闲页派出去。 */
    .set PML4_ADDR, 0x4000
    .set PDPT_ADDR, 0x5000
    .set PD_ADDR,   0x6000

_setup_start:
    movw    %cs, %ax
    movw    %ax, %ds
    movw    %ax, %ss
    movw    $0x3000, %sp

/*
 * 收集机器参数到 0x90000（偏移沿用原版布局）
 */
    movw    $INITSEG, %ax
    movw    %ax, %es

    movb    $0x03, %ah          /* 读光标位置 -> 0x90000 */
    xorb    %bh, %bh
    int     $0x10
    movw    %dx, %es:0

    movb    $0x88, %ah          /* 扩展内存大小(KB) -> 0x90002 */
    int     $0x15
    movw    %ax, %es:2

    movb    $0x0f, %ah          /* 显示页/模式/列数 -> 0x90004..6 */
    int     $0x10
    movw    %bx, %es:4
    movw    %ax, %es:6

/* 硬盘参数表 hd0 -> 0x90080（原版从中断向量 0x41 指向的表里拷 16 字节）*/
    pushw   %ds
    xorw    %ax, %ax
    movw    %ax, %ds
    movw    (0x41*4), %si
    movw    (0x41*4+2), %ax
    movw    %ax, %ds
    movw    $0x0080, %di
    movw    $8, %cx
    cld
    rep     movsw
    popw    %ds

/*
 * E820 内存探测 -> 0x901E0 存条目数, 0x90200 起存条目
 * 原版内核只有 int 0x15/0x88，这里为 64 位内核补上 E820。
 */
    movw    $INITSEG, %ax
    movw    %ax, %es
    xorl    %ebx, %ebx
    xorw    %bp, %bp
    movw    $0xF000, %di        /* es:di = 0x9F000, 避开 setup 自身与 kernel BSS */
e820_loop:
    movl    $0x0000E820, %eax
    movl    $0x534D4150, %edx   /* 'SMAP' */
    movl    $20, %ecx
    int     $0x15
    jc      e820_done
    cmpl    $0x534D4150, %eax
    jne     e820_done
    incw    %bp
    addw    $20, %di
    cmpw    $0xF000 + 20*128, %di
    jae     e820_done
    testl   %ebx, %ebx
    jnz     e820_loop
e820_done:
    movw    %bp, %es:0x1E0

/* 屏蔽全部中断源: 8259 + CPU（同原版思路，进保护模式前必须静默）*/
    cli
    movb    $0xFF, %al
    outb    %al, $0x21
    call    io_delay
    outb    %al, $0xA1
    call    io_delay

/* 打开 A20（原版做法：8042 命令 0xD1 + 0xDF）*/
    call    empty_8042
    movb    $0xD1, %al
    outb    %al, $0x64
    call    empty_8042
    movb    $0xDF, %al
    outb    %al, $0x60
    call    empty_8042
/* 再补一发 Fast A20，兼容不响应 8042 的芯片组 */
    inb     $0x92, %al
    orb     $0x02, %al
    andb    $0xFE, %al          /* 不要误触发热复位 */
    outb    %al, $0x92

/*
 * 建立 4 级页表: 恒等映射低端 1GB, 使用 2MB 大页
 *   PML4[0] -> PDPT, PDPT[0] -> PD, PD[i] = i*2MB | PS|RW|P
 */
    xorl    %eax, %eax
    movw    $(PML4_ADDR >> 4), %ax
    movw    %ax, %es
    xorw    %di, %di
    movw    $(4096*3/2), %cx    /* 清零 3 个页 */
    xorw    %ax, %ax
    cld
    rep     stosw

    movw    $(PML4_ADDR >> 4), %ax
    movw    %ax, %es
    movl    $(PDPT_ADDR + 0x03), %es:0      /* present | rw */
    movw    $(PDPT_ADDR >> 4), %ax
    movw    %ax, %es
    movl    $(PD_ADDR + 0x03), %es:0

    movw    $(PD_ADDR >> 4), %ax
    movw    %ax, %es
    xorw    %di, %di
    movl    $0x00000183, %eax   /* global | PS(2MB) | rw | present */
    movw    $512, %cx
1:  movl    %eax, %es:(%di)
    movl    $0, %es:4(%di)
    addl    $0x00200000, %eax
    addw    $8, %di
    loop    1b

/* 装载 GDT / 清空 IDT（原版是 lgdt gdt_48 / lidt idt_48）*/
    movw    %cs, %ax
    movw    %ax, %ds
    lgdt    gdt_desc
    lidt    idt_desc

/* CR4.PAE | CR4.PGE | CR4.OSFXSR | CR4.OSXMMEXCPT
 * OSFXSR(0x200)+OSXMMEXCPT(0x400) 让 SSE/SSE2 指令可用并把 SSE 异常
 * 导向 #XM 而非 #UD；不开的话任何访问 XMM 的指令（glibc 启动里很多，
 * 例如 movd %ebx,%xmm0）会触发 #UD->SIGILL，用户程序无法启动。 */
    movl    %cr4, %eax
    orl     $0x6A0, %eax
    movl    %eax, %cr4

/* CR3 = PML4 */
    movl    $PML4_ADDR, %eax
    movl    %eax, %cr3

/* EFER.LME = 1, EFER.SCE = 1 (syscall enable), EFER.NXE = 1 (NX bit)。
 * NXE 必须开：mprotect 给 .data/.bss 设 NO_EXEC(位63) 时，若 NXE 关闭，
 * 位63 是保留位，CPU 一访问就 #PF(reserved) 把 glibc 启动卡死。 */
    movl    $0xC0000080, %ecx
    rdmsr
    orl     $0x901, %eax
    wrmsr

/* 同时开 PG 和 PE，直接进入 long mode 的 compatibility 状态。
 * 必须同时置位 WP（CR0 bit 16）：WP=0 时内核态写忽略页表 R/W 位，
 * fork 后所有用户页都被 cow_copy_page_table 标成只读，此时内核的
 * copy_to_user（read 等）与 clone 的 CLONE_CHILD_SETTID/CLEARTID
 * 会直接写到共享物理页、绕过写时复制，把父或子的堆/TLS 数据写穿，
 * 表现为 fork 出的子进程（ls/cat）在 glibc malloc 里取到腐败 chunk 而 #GP。
 * Linux 在 start_kernel 早期就置 WP=1，这里对齐。 */
    movl    %cr0, %eax
    orl     $0x80010001, %eax
    movl    %eax, %cr0

/* 远跳进 64 位代码段，落到 system (物理 0x10000) */
    .byte   0x66, 0xEA          /* ljmpl $0x08, $0x10000 */
    .long   0x00010000
    .word   0x08

io_delay:
    outb    %al, $0x80
    ret

empty_8042:
    call    io_delay
    inb     $0x64, %al
    testb   $0x01, %al          /* 输出缓冲有数据? 读掉 */
    jz      1f
    call    io_delay
    inb     $0x60, %al
    jmp     empty_8042
1:  testb   $0x02, %al          /* 输入缓冲满? 继续等 */
    jnz     empty_8042
    ret

/* ---- 描述符表 ---- */
    .align 16
gdt:
    .quad   0x0000000000000000  /* NULL */
    .quad   0x00AF9A000000FFFF  /* 0x08: 64 位代码段, L=1 */
    .quad   0x00CF92000000FFFF  /* 0x10: 数据段 */
gdt_end:

gdt_desc:
    .word   gdt_end - gdt - 1
    .long   0x90200 + gdt       /* setup 被加载在 0x90200 */

idt_desc:
    .word   0
    .long   0
