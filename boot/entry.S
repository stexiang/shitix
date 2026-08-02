/*
 * boot/entry.S
 *
 * 参照 linux-1.0.9 的 linux/kernel/sys_call.S 重写为 64 位。
 *
 * 原版这一个文件同时承担四件事，这里保持同样的分工：
 *   1. SAVE_ALL / RESTORE_ALL —— 统一的寄存器保存布局（原版同名宏）
 *   2. 异常入口桩 divide_error..alignment_check（原版 .globl 那一长串）
 *   3. IRQ 入口桩 IRQ0..IRQ15（原版靠 irq.h 的 BUILD_IRQ 宏生成）
 *   4. system_call（int 0x80 入口）与 ret_from_sys_call
 * 外加 64 位特有的 switch_to：原版靠 TSS 硬件任务切换（ljmp），
 * long mode 取消了这个机制，必须软件保存/恢复。
 *
 * pt_regs 栈布局（对应原版开头那段注释）。地址从低到高，
 * 与 src/traps.rs 的 `struct PtRegs` 字段顺序必须逐项一致：
 *
 *	 0(%rsp) - %r15		 \
 *	 8(%rsp) - %r14		  |
 *	10(%rsp) - %r13		  | callee-saved，switch_to 也要用
 *	18(%rsp) - %r12		  |
 *	20(%rsp) - %rbp		  |
 *	28(%rsp) - %rbx		 /
 *	30(%rsp) - %r11		 \
 *	38(%rsp) - %r10		  |
 *	40(%rsp) - %r9		  |
 *	48(%rsp) - %r8		  |
 *	50(%rsp) - %rax		  | caller-saved
 *	58(%rsp) - %rcx		  |
 *	60(%rsp) - %rdx		  |
 *	68(%rsp) - %rsi		  |
 *	70(%rsp) - %rdi		 /
 *	78(%rsp) - orig_rax	  系统调用号 / 异常错误码 / IRQ 号取反
 *	80(%rsp) - %rip		 \
 *	88(%rsp) - %cs		  |
 *	90(%rsp) - %rflags	  | 由 CPU 压入
 *	98(%rsp) - %rsp		  |
 *	a0(%rsp) - %ss		 /
 *
 * 注意 orig_rax 这一格的三用途沿袭原版：原版用它存 orig_eax（系统调用号），
 * 异常路径存 error_code，IRQ 路径存 ~irq。Rust 侧按向量号区分解释。
 */
    .code64

/* 段选择子，与 src/gdt.rs 的 GDT 布局一致 */
    .set KERNEL_CS, 0x08
    .set KERNEL_DS, 0x10

/* pt_regs 里各字段的偏移，供下面几处直接寻址 */
    .set PT_RBX,      0x28
    .set PT_RAX,      0x50
    .set PT_RDI,      0x70
    .set PT_ORIG_RAX, 0x78
    .set PT_RIP,      0x80
    .set PT_CS,       0x88
    .set PT_RFLAGS,   0x90

    .set RFLAGS_IF, 0x200
    .set RFLAGS_NT, 0x4000

/*
 * SAVE_ALL —— 对应原版同名宏。进入时栈上已有 CPU 压的 5 个字
 * 加上一格 orig_rax（错误码 / 系统调用号 / ~irq）。
 * long mode 下 ds/es/ss/fs/gs 的选择子对数据访问无效（段基址强制为 0），
 * 所以原版那几句 push %gs / mov %dx,%ds 全部不需要，省掉。
 */
    .macro SAVE_ALL
    cld
    pushq   %rdi
    pushq   %rsi
    pushq   %rdx
    pushq   %rcx
    pushq   %rax
    pushq   %r8
    pushq   %r9
    pushq   %r10
    pushq   %r11
    pushq   %rbx
    pushq   %rbp
    pushq   %r12
    pushq   %r13
    pushq   %r14
    pushq   %r15
    .endm

/*
 * RESTORE_ALL —— 对应原版同名宏。最后那句 addl $4,%esp 是丢掉 orig_eax
 * 这一格，64 位下换成 addq $8。
 */
    .macro RESTORE_ALL
    popq    %r15
    popq    %r14
    popq    %r13
    popq    %r12
    popq    %rbp
    popq    %rbx
    popq    %r11
    popq    %r10
    popq    %r9
    popq    %r8
    popq    %rax
    popq    %rcx
    popq    %rdx
    popq    %rsi
    popq    %rdi
    addq    $8, %rsp
    iretq
    .endm

/* ---- 异常入口桩（对应原版 .globl _divide_error,_debug,... 那一串）---- */

/*
 * 无错误码的异常：CPU 不压 error_code，自己补一格 0 占住 orig_rax，
 * 让 pt_regs 布局对所有向量保持一致（原版靠 pushl $0 做同样的事）。
 */
    .macro EXC_NOERR name, vector
    .global \name
\name:
    pushq   $0
    pushq   $\vector
    jmp     exc_common
    .endm

/* 有错误码的异常：CPU 已经压好 error_code，直接接向量号 */
    .macro EXC_ERR name, vector
    .global \name
\name:
    pushq   $\vector
    jmp     exc_common
    .endm

    .section .text

    EXC_NOERR divide_error,                    0
    EXC_NOERR debug,                           1
    EXC_NOERR nmi,                             2
    EXC_NOERR int3,                            3
    EXC_NOERR overflow,                        4
    EXC_NOERR bounds,                          5
    EXC_NOERR invalid_op,                      6
    EXC_NOERR device_not_available,            7
    EXC_ERR   double_fault,                    8
    EXC_NOERR coprocessor_segment_overrun,     9
    EXC_ERR   invalid_TSS,                    10
    EXC_ERR   segment_not_present,            11
    EXC_ERR   stack_segment,                  12
    EXC_ERR   general_protection,             13
    EXC_ERR   page_fault,                     14
    EXC_NOERR reserved,                       15
    EXC_NOERR coprocessor_error,              16
    EXC_ERR   alignment_check,                17
    EXC_NOERR machine_check,                  18
    EXC_NOERR simd_coprocessor_error,         19
    EXC_NOERR virtualization,                 20

/*
 * exc_common —— 原版每个向量有独立的 do_xxx，靠 DO_ERROR 宏批量生成；
 * 这里把向量号一路传到 Rust，由 traps::do_trap 一处分发（等价，且省掉
 * 21 份几乎相同的代码）。
 *
 * 进入时栈顶是刚压的向量号，紧下面是 orig_rax 格（错误码）。
 * 先把向量号弹到 rax 暂存，SAVE_ALL 之后再作为第二参数传出去。
 */
exc_common:
    popq    %rax                    /* 向量号 */
    SAVE_ALL
    movq    %rsp, %rdi              /* 参数 1: &mut PtRegs */
    movq    %rax, %rsi              /* 参数 2: 向量号 */
    /* SysV ABI 要求 call 时 rsp 16 字节对齐。此刻 rsp 指向 pt_regs 起点，
       pt_regs 是 0xa8 字节且 iretq 帧本身 8 字节对齐，所以对齐性取决于
       进入异常时的栈。统一用 rbp 对齐一次，代价一条指令。 */
    movq    %rsp, %rbp
    andq    $-16, %rsp
    call    do_trap
    movq    %rbp, %rsp
    jmp     ret_from_sys_call

/* ---- IRQ 入口桩（对应原版 irq.h 的 BUILD_IRQ 宏）---- */

/*
 * 原版为每条 IRQ 线生成三个桩：完整版 IRQn_interrupt、快速版
 * fast_IRQn_interrupt、以及未注册时的 bad_IRQn_interrupt，靠 sa_flags 的
 * SA_INTERRUPT 选择。我们只生成一份完整版，把「快速/未注册」的区分挪到
 * Rust 侧 irq::do_irq 里判断——桩代码是热路径但那点分支代价远小于
 * 维护三套汇编的复杂度，而且我们暂时没有需要 SA_INTERRUPT 的驱动。
 *
 * orig_rax 格存 ~irq（原版 BUILD_IRQ 也是 pushl $~(nr)），这样 Rust 侧
 * 能靠符号区分「这格是错误码还是 IRQ 号」。
 */
    .macro BUILD_IRQ nr
    .global IRQ\nr\()_interrupt
IRQ\nr\()_interrupt:
    pushq   $~\nr
    pushq   $\nr
    jmp     irq_common
    .endm

    BUILD_IRQ 0
    BUILD_IRQ 1
    BUILD_IRQ 2
    BUILD_IRQ 3
    BUILD_IRQ 4
    BUILD_IRQ 5
    BUILD_IRQ 6
    BUILD_IRQ 7
    BUILD_IRQ 8
    BUILD_IRQ 9
    BUILD_IRQ 10
    BUILD_IRQ 11
    BUILD_IRQ 12
    BUILD_IRQ 13
    BUILD_IRQ 14
    BUILD_IRQ 15

irq_common:
    popq    %rax                    /* IRQ 号 */
    SAVE_ALL
    movq    %rax, %rdi              /* 参数 1: irq */
    movq    %rsp, %rsi              /* 参数 2: &mut PtRegs（原版 do_IRQ 同序）*/
    movq    %rsp, %rbp
    andq    $-16, %rsp
    call    do_IRQ
    movq    %rbp, %rsp
    jmp     ret_from_sys_call

/* ---- 系统调用（对应原版 _system_call）---- */

/*
 * int 0x80 入口。原版把系统调用号放 eax、参数放 ebx/ecx/edx/esi/edi/ebp。
 * 我们沿用「号在 rax」，但参数改用 SysV 的 rdi/rsi/rdx/r10/r8/r9
 * （即 Linux x86_64 的约定，r10 代替 rcx 因为 syscall 指令会毁掉 rcx），
 * 这样 syscall 指令路径和 int 0x80 路径能共用同一个 Rust 分发函数。
 */
    .global system_call
system_call:
    pushq   %rax                    /* orig_rax = 系统调用号，同原版 */
    SAVE_ALL
    movq    %rsp, %rdi              /* 参数 1: &mut PtRegs */
    movq    %rsp, %rbp
    andq    $-16, %rsp
    call    do_syscall              /* 返回值由 Rust 侧写回 pt_regs.rax */
    movq    %rbp, %rsp
    jmp     ret_from_sys_call

/*
 * syscall 指令入口（x86_64 特有，原版没有）。
 * 进入时：rcx = 用户 rip，r11 = 用户 rflags，且**没有换栈**——
 * 必须自己从 TSS 里取内核栈。原版 int 0x80 由 CPU 通过 TSS.ss0/esp0 换栈，
 * 这里要手工做。
 *
 * 暂未启用（需要 per-cpu 的用户栈暂存位置，等 sched 的 percpu 就位）。
 * 保留符号以便 syscall::init 决定是否写 MSR_LSTAR。
 */
    .global syscall_entry
syscall_entry:
    /* 目前 MSR_LSTAR 未指向这里，走到这说明配置错了，直接停机比乱跑安全 */
    cli
    hlt
    jmp     syscall_entry

/* ---- 返回路径（对应原版 ret_from_sys_call）---- */

/*
 * 原版这段做四件事：跑 bottom half、必要时 schedule()、投递信号、
 * RESTORE_ALL + iret。我们保留前两件和最后一件；信号投递等 signal.c
 * 移植后再接（那时在这里插一个 do_signal 调用即可）。
 *
 * 原版还有一段「内核态返回就跳过这些检查」的逻辑（cmpw $KERNEL_CS,CS(%esp)），
 * 必须保留：在内核态的异常返回路径上调度会破坏内核栈上的调用链。
 */
    .global ret_from_sys_call
ret_from_sys_call:
    /* 中断嵌套中（intr_count != 0）不做任何调度/软中断，直接返回。
       对应原版 `cmpl $0,_intr_count; jne 2f`。 */
    cmpq    $0, intr_count(%rip)
    jne     restore_and_iret

    /* bottom half：对应原版 `movl _bh_mask,%eax; andl _bh_active,%eax` */
    movq    bh_active(%rip), %rax
    andq    bh_mask(%rip), %rax
    jnz     handle_bottom_half
ret_check_resched:
    /* 只有返回用户态时才允许调度（原版 cmpw $KERNEL_CS,CS(%esp); je 2f）*/
    cmpw    $KERNEL_CS, PT_CS(%rsp)
    je      restore_and_iret

    /* 强制打开 IF、清掉 NT，防止用户态传进来一个坏 rflags
       （对应原版 orl $IF_MASK / andl $~NT_MASK 那两句）*/
    movq    PT_RFLAGS(%rsp), %rax
    orq     $RFLAGS_IF, %rax
    andq    $~RFLAGS_NT, %rax
    movq    %rax, PT_RFLAGS(%rsp)

    cmpl    $0, need_resched(%rip)
    jne     reschedule
restore_and_iret:
    RESTORE_ALL

/* 原版 reschedule: 把 ret_from_sys_call 压栈后 jmp _schedule */
reschedule:
    call    schedule
    jmp     ret_check_resched

/* 原版 handle_bottom_half：开中断跑软中断，期间 intr_count 加一 */
handle_bottom_half:
    incq    intr_count(%rip)
    sti
    movq    %rsp, %rbp
    andq    $-16, %rsp
    call    do_bottom_half
    movq    %rbp, %rsp
    cli
    decq    intr_count(%rip)
    jmp     ret_check_resched

/* ---- 任务切换（64 位特有，原版是 TSS 硬件切换）---- */

/*
 * switch_to(prev_sp: *mut u64, next_sp: u64)
 *   rdi = 存放当前 rsp 的位置（写回 prev.tss.rsp）
 *   rsi = 要切到的 rsp（next.tss.rsp）
 *
 * 原版 `switch_to(next)` 展开成一条 `ljmp` 到 next 的 TSS 描述符，
 * 由 CPU 自动保存/恢复全部寄存器。long mode 删掉了这个机制（TSS 只剩
 * 栈指针表），所以必须软件保存 callee-saved 寄存器 + 换栈。
 *
 * cr3 不在这里切：我们目前所有任务共用内核页表，等 fork 真正复制页表后
 * 由调用方（sched::switch_context）在调用前后处理。
 */
    .global switch_to
switch_to:
    pushq   %rbp
    pushq   %rbx
    pushq   %r12
    pushq   %r13
    pushq   %r14
    pushq   %r15
    pushfq
    movq    %rsp, (%rdi)            /* prev.tss.rsp = 当前 rsp */
    movq    %rsi, %rsp              /* 换到 next 的内核栈 */
    popfq
    popq    %r15
    popq    %r14
    popq    %r13
    popq    %r12
    popq    %rbx
    popq    %rbp
    ret                             /* 回到 next 上次 switch_to 的返回点 */

/*
 * 新任务的第一次入场点（对应原版 fork.c 里
 * `p->tss.eip = (unsigned long) ret_from_sys_call`）。
 * copy_process 把子进程内核栈布置成「switch_to 的保存区 + pt_regs」，
 * 返回地址填这里，于是 switch_to 的 ret 会落到此处，
 * 然后走正常返回路径进入用户态（或内核线程的入口）。
 */
    .global ret_from_fork
ret_from_fork:
    movq    %rsp, %rbp
    andq    $-16, %rsp
    call    schedule_tail           /* 让 Rust 侧完成子进程的收尾统计 */
    movq    %rbp, %rsp
    jmp     ret_from_sys_call

/*
 * 内核线程入场点：栈上依次是 fn 和 arg，调用 fn(arg) 后不返回
 * （原版没有内核线程，1.0.9 的 init 是 fork 出来后 execve 的）。
 */
    .global kernel_thread_entry
kernel_thread_entry:
    call    schedule_tail
    popq    %rax                    /* fn */
    popq    %rdi                    /* arg */
    andq    $-16, %rsp
    call    *%rax
    /* 内核线程返回视为退出 */
    movq    $0, %rdi
    call    do_kthread_exit
    cli
1:  hlt
    jmp     1b
