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
 */
exc_common:
    /*
     * 不能像原来那样 `popq %rax` 取向量号：那会在 SAVE_ALL **之前**毁掉被
     * 打断上下文的 %rax——IRQ 路径上同样的写法就是 bug-029 的根因（见
     * irq_common 的注释）。异常路径上这条一直是潜伏的，因为目前绝大多数
     * 异常直接 panic；但 int3/debug/overflow 本来就可恢复，而 page_fault
     * 在用户态与 COW 接上之后会变成高频可恢复异常，那时每次缺页都会把
     * 被打断代码的 %rax 换成向量号 14。
     *
     * 改成先 SAVE_ALL（此时 %rax 还是真值，被如实存进 PT_RAX），向量号留在
     * pt_regs 上方的临时槽里，保存完再取。
     */
    SAVE_ALL                        /* 此刻 0x78(%rsp)=向量号, 0x80=错误码 */
    movq    0x78(%rsp), %rsi        /* 参数 2: 向量号 */
    /*
     * 把「错误码 + CPU 压的 iretq 帧」整体下移 8 字节，盖掉那个临时槽，
     * 让栈恢复成标准 pt_regs 布局（orig_rax 回到 PT_ORIG_RAX = 0x78）。
     * 从低地址往高地址依次搬，源永远在目标上方 8 字节，不会自我覆盖。
     * 这里拿 %rax 当搬运寄存器是安全的：它的真值已经在 PT_RAX 里，
     * RESTORE_ALL 从那一格恢复，我们不碰那一格。
     */
    movq    0x80(%rsp), %rax        /* 错误码 → orig_rax */
    movq    %rax, 0x78(%rsp)
    movq    0x88(%rsp), %rax        /* rip */
    movq    %rax, 0x80(%rsp)
    movq    0x90(%rsp), %rax        /* cs */
    movq    %rax, 0x88(%rsp)
    movq    0x98(%rsp), %rax        /* rflags */
    movq    %rax, 0x90(%rsp)
    movq    0xa0(%rsp), %rax        /* rsp */
    movq    %rax, 0x98(%rsp)
    movq    0xa8(%rsp), %rax        /* ss */
    movq    %rax, 0xa0(%rsp)
    movq    %rsp, %rdi              /* 参数 1: &mut PtRegs */
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
    /*
     * bug-029：这里原本是「先 `pushq $nr` 传 IRQ 号，再在 irq_common 里
     * `popq %rax` 取回」——而那条 pop **在 SAVE_ALL 之前**，把被打断任务的
     * %rax 就地毁掉了，SAVE_ALL 存下去的是 IRQ 号，RESTORE_ALL 又老实把它
     * 恢复回去。于是每个时钟滴答都让被打断的内核代码带着 %rax = 0 继续跑
     * （时钟是 IRQ 0）。低 1GB 是恒等映射，所以那个 0 当指针用时不 page
     * fault，而是静默读回实模式 IVT 的字节——`0xf000ff53` 那串 BIOS IRET
     * 桩，表现为 i_mode=0xff53 / nlink=255 / s_magic=0xff53；当计数用时
     * 就是「readdir 列出 0 项」。%rax 在滴答那一刻是否活着完全取决于寄存器
     * 分配，所以改一行无关代码都能让失败率大幅跳动。
     *
     * orig_rax 那一格本来就存着 ~nr，SAVE_ALL 之后从 PT_ORIG_RAX 取出来取反
     * 即可，不需要额外的 scratch push，也就不需要在保存前动任何寄存器。
     */
    SAVE_ALL
    movq    PT_ORIG_RAX(%rsp), %rdi /* 参数 1: irq = ~orig_rax */
    notq    %rdi
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
	 * 这里要手工换栈 + swapgs。
	 * 用全局 scratch 变量代替 per-cpu 区域存用户 RSP，
	 * kernel_rsp_scratch 在 switch_to_task 里更新。
	 */
	    .global syscall_entry
	syscall_entry:
	    swapgs
	    movq    %rsp, user_rsp_scratch(%rip)
	    movq    kernel_rsp_scratch(%rip), %rsp
	    pushq   $0x23
	    pushq   user_rsp_scratch(%rip)
	    pushq   %r11
	    pushq   $0x1B
	    pushq   %rcx
	    pushq   %rax
	    cld
	    SAVE_ALL
	    call    do_syscall
	    jmp     ret_from_sys_call

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

    /*
     * 投递待处理信号。对应原版 ret_from_sys_call 里那段
     *   movl _current,%eax; cmpl $0,signal(%eax); je 2f
     *   ... pushl %esp; call _do_signal
     *
     * 原版直接在汇编里读 task_struct 的 signal/blocked 偏移；我们的
     * `current` 是个下标（sched::CURRENT）而不是指针，且 Task 的字段偏移
     * 由 Rust 布局决定，写死在汇编里会随 Task 改字段而静默错位。
     * 所以先 call 一个 Rust 谓词（has_pending_signal），真有信号才进
     * do_signal——常态路径多一次 call，换来偏移不会失同步。
     */
    movq    %rsp, %rbp
    andq    $-16, %rsp
    call    signal_pending_c
    movq    %rbp, %rsp
    testb   %al, %al
    jz      restore_and_iret

    /* 同 reschedule：call 之前 rsp 必须 16 对齐（见那里的注释）。
       do_signal 的唯一参数是 pt_regs 指针，也就是对齐前的 rsp。 */
    movq    %rsp, %rbp
    movq    %rsp, %rdi
    andq    $-16, %rsp
    call    do_signal
    movq    %rbp, %rsp
    /* do_signal 可能把状态改成 Stopped 并 schedule 过，重新走一遍检查 */
    jmp     ret_check_resched
restore_and_iret:
    RESTORE_ALL

/*
 * 原版 reschedule: 把 ret_from_sys_call 压栈后 jmp _schedule。
 *
 * 这里必须先对齐栈再 call：pt_regs 是 0xa8 字节（8 mod 16），所以进到
 * ret_from_sys_call 时 rsp 是 8 mod 16，直接 call 会让 schedule 拿到一个
 * 不满足 SysV ABI 的栈。opt-level=1 下 LLVM 会用 movaps 存取栈上的槽位，
 * 栈错位时那些访问要么 #GP 要么读到错的位置——表现出来就是「某个基址
 * 寄存器莫名变成 0」（bug-029）。其余几处 call 都已经这么做了，
 * 唯独这里漏了。
 */
reschedule:
    movq    %rsp, %rbp
    andq    $-16, %rsp
    call    schedule
    movq    %rbp, %rsp
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
    /* 同 reschedule：call 之前 rsp 必须 16 对齐（见那里的注释）。 */
    movq    %rsp, %rbp
    andq    $-16, %rsp
    call    schedule_tail
    movq    %rbp, %rsp
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

/* ---- syscall scratch 变量 ---------------------------------- */
    .section .bss
    .align 8
    .global user_rsp_scratch
user_rsp_scratch:
    .quad 0
    .global kernel_rsp_scratch
kernel_rsp_scratch:
    .quad 0
