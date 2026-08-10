# Cerebrum

> OpenWolf's learning memory. Updated automatically as the AI learns from interactions.
> Do not edit manually unless correcting an error.
> Last updated: 2026-08-01

## User Preferences

- 用中文交流；代码注释也用中文。
- 每个 `unsafe` 块上方必须有 `// SAFETY:` 注释说明前提，`unsafe fn` 要写 `# Safety` 文档段。
- 写新模块时对照 `linux/` 下的原始 C 代码，并在文档注释里点明对应的原版文件/函数。

## Key Learnings

- **Project:** shitix — 用 Rust 重写 Linux 1.0.9，x86_64，QEMU。
- 引导链是自己写的：`bootsect.S` → `setup.S` → `head.S` → `call start_kernel`，内核编成 **staticlib** 由 `ld` 链接，**不用 bootimage/bootloader crate**。
- `head.S` 用 SysV ABI 把 `0x90000` 放进 `rdi` 传给 `start_kernel`，所以入口签名是 `extern "C" fn(*const BootParams) -> !`。
- edition 2024：`#[no_mangle]` 要写成 `#[unsafe(no_mangle)]`；直接借用 `static mut` 被禁，用 `core::ptr::addr_of_mut!`。
- `test.sh` 靠串口里的 `SHITIX_BOOT_OK` 判定成功；内核以 `hlt` 空转，所以正常路径必然被 `timeout` 杀掉（退出码 124 视为正常）。
- QEMU 挂盘用 `if=ide`（不是 floppy），`-m 256M`。
- 工具链固定 nightly（`rust-toolchain.toml`），依赖 `x86_64` crate（`--no-default-features --features instructions,const_fn`）。
- **E820 的 usable 区间包含低 1MB 里的启动期结构**（0x70000 页表、0x90000 参数区、0x9E000 E820 数组），不能直接全部当空闲内存——这是 bug-001 的根源。
- setup.S 只恒等映射了低 1GB，所以 mm 必须把可管理内存 clamp 到 `1<<30`；给 QEMU `-m 3G` 也只认 1GB。
- 启动期诊断信息用 `kprintln!`（同时写 VGA 和串口），这样 `test.sh` 的日志能抓到布尔断言，不必靠截屏 OCR。
- 验证内核逻辑的有效手法：写一段临时自检/压力测试跑一遍看串口输出，通过后删掉（比只读代码可靠）。
- QEMU 因内核 `hlt` 空转而不退出，管道里接 `grep` 会因缓冲拿不到输出——要用 `-serial file:xxx.log` 落盘再 grep。
- 查三重错误用 `qemu ... -d cpu_reset`，能直接看到 CPU Reset 与当时的寄存器状态。
- 验证 VGA 输出的方法：`(sleep 6; echo "screendump /tmp/x.ppm"; sleep 2; echo quit) | qemu ... -monitor stdio`，然后用 PIL 按 9x16 单元格逐行统计像素颜色。文本模式实际分辨率是 720x400。

- klib 移植的边界：`compiler_builtins` 的 `mem` 特性已经为 `x86_64-unknown-none` 提供了 `memcpy`/`memset`/`memcmp` 等符号，`src/klib/string.rs` 里**不能**再 `#[unsafe(no_mangle)]` 导出同名函数，否则链接期撞符号。需要编译器内建版本时用 `core::ptr::copy_nonoverlapping` / `write_bytes`。
- 原版 `lib/` 里除 `ctype.c`/`string.c`/`errno.c` 外全是**用户态**系统调用桩（`_syscall0/1/3` 展开成 `int $0x80`），给内核里那个 `init()` 用；要等系统调用入口和进程模型到位才能移植，届时放 `src/syscall/` 而不是 `src/klib/`。`lib/malloc.c` 在 1.0.9 里是空文件。
- 移植 C 时要留意 `while(cond--)` 这类**带副作用的循环条件**：C 版退出后变量已被消耗，后续分支依赖这个状态。照着语义直译成 `if x > 0 {...}` 会漏掉副作用（bug-002 就是这么来的）。
- `printk` 的级别前缀就是消息文本开头的 `"<N>"`（`KERN_ERR` 等宏本体是字符串常量），解析必须在 vsprintf 之后做，因为级别可能来自格式化结果。

- **系统调用接到 fs 层（模块 7，2026-08-06）:** 21 个调用从 `-ENOSYS` 占位改成真接 fs 层：`open`/`creat`/`close`/`read`、`dup`/`dup2`、`chdir`/`chmod`/`truncate`、`mkdir`/`rmdir`/`unlink`/`link`/`mknod` → `fs::namei::do_*`、`fsync`、`stat`/`lstat`/`fstat` → `fs::stat::*`（经 `user_stat_out` 写用户态）、`getdents`/`getdents64`。`write` 改成 fs 优先 + 控制台兜底（先走 `fs::read_write::write`，fd 1/2 拿到 `-EBADF` 才退回内核控制台，因为 task[0] 没有打开的 stdout/stderr）。
- **用户指针护栏（临时方案，必须替换）:** `check_range`/`user_path`/`user_buf`/`user_buf_mut`/`user_stat_out` 只接受恒等映射低 1GB 内的地址，路径按 PATH_MAX 4096 有界 strnlen。⚠️ **它不阻止用户态读写内核内存**（低 1GB 里内核和用户态共享同一段恒等映射），只挡未映射地址；等 `mm/mmap.c` 的 `vm_area_struct` 移植完必须换成真的 `verify_area`。
- **内核栈池移出 BSS（2026-08-06）:** `.text`/`.rodata` 涨约 24KB 后 `_kernel_end` 冲到 0x922C0，超 0x90000 共 8896 字节。`sched::KSTACKS`（48KB，BSS 最大项）改成由 `page_alloc::init` 在 `page_ref` 表之后划出、清零、调 `sched::attach_kstacks()`（照 bug-024 的 `page_ref::attach()` 套路），`map_end` 抬到池尾之后确保不进空闲链表；`MemInfo` 加 `kstack_addr`。顺带解掉一个长期限制：`KSTACK_SLOTS` **3 → 8**，原注释写明「再加一份就会越界」，现在池子不占 BSS 不再卡内核线程数。修完后 `_kernel_end` = 0x854F0 (debug) / 0x5B4E0 (release)，距 0x90000 约 44KB 余量。
- **`scripts/check-syscall-nr.py` 机器核对（2026-08-06）:** 可执行脚本，解析 `nr` 模块的 `pub const X: usize = N` 对比内核头的 `#define __NR_x N`，自动找 `/usr/src/linux-headers-*/arch/x86/include/generated/uapi/asm/unistd_64.h`，带 `PRIVATE` 白名单（idle/unused/nr_syscalls/umount/prlimit/setmempolicy/getmempolicy）和 `sysctl`→`_sysctl` 拼写映射，另自查重号。输出 `官方 360 个号，本树 360 个（另有 3 个私有/别名）/ 全部一致`，exit 0。加新调用号一律照正式表填，不要按功能分组手写——那样必然重号（见 buglog bug-026）。
- **`scripts/test.sh` 加 `MEM=` 覆盖（2026-08-06）:** 默认 256M，照 `TIMEOUT`/`PROFILE` 的既有约定，用来跑内存矩阵（32M/128M/1G/3G 全部启动且 `syscall-fs` 绿）。
- **LFS 集成的真实结论（2026-08-06，读代码核实过）:** 用户问「能否集成进 LFS 系统」。**当前内核跑不了 LFS 用户态**，缺的不是零碎补丁：完全没有 ring-3 切换（`USER_CS` 只出现在常量定义里，全树没有任何地方 `iretq` 到用户态——内核从未执行过一条用户态指令）；`sys_execve` 是 `-ENOSYS` 占位；ELF 加载器只认 32 位（`parse_elf32`/`Elf32Header`，LFS 的 x86_64 二进制是 ELF64）；没有 `copy_from_user`/`copy_to_user`/`verify_area`/`access_ok`；`fs::stat::Stat` 是 1.0.9 的 i386 布局（`u16 st_dev`、`u32 st_ino/st_size`），不是 x86_64 glibc 的 `struct stat`——对真实用户态是 ABI 不匹配；`arch_prctl` 是占位（glibc 靠 `ARCH_SET_FS` 装 TLS），且没有 per-task FS/GS base；`clone` 的 flags 语义、信号投递到用户态、`fork` 的页表复制都缺。

- **BSS 预算只剩约 44KB**（2026-08-06：`_kernel_end`=0x854F0，上限 0x90000，模块 7 修完后）。下一个加静态缓冲的模块前要先 `nm target/boot/system.elf | grep _kernel_end` 复核。已知的剩余放血点：`desc.rs` 的三个 8KB IST 栈（DF/NMI/PF，共 24KB）。
- 排查 BSS 超限的最快路径：把 `kernel.ld` 的 ASSERT 注释掉链一份到 `/tmp`，再 `nm --size-sort -r -S` 过滤 `[bB]` 看谁最大。单看 `libshitix.a` 的 `nm` 会漏掉泛型实例化后的真实尺寸，要看链接后的 ELF。
- `.wolf/config.json` 的 `anatomy.max_files` 默认 500，而 `linux/` 参考树本身就有 ~490 个文件，**把我们自己的 `src/` 全挤出索引了**（`grep src/ anatomy.md` 一无所获）。已调到 900，重扫得 594 个文件。以后新增源码目录后要确认它真的出现在 anatomy.md 里。

- **long mode 砍掉的三个 386 机制**，移植 1.0.9 时每个都要替代方案：(1) TSS 硬件任务切换（`ljmp` 到 TSS 描述符）→ 必须软件保存 callee-saved + 换 rsp，全系统只需**一个** TSS，`rsp0` 每次调度改写；(2) 段基址（原版靠 `USER_DS` 的基址实现 3GB 用户空间分割）→ 只能靠页表，数据段描述符只剩 DPL+W 有意义；(3) 32 位没有 IST → 64 位应该给 double fault / NMI / page fault 各配独立栈，否则栈溢出直接升级成三重错误。
- 64 位代码段描述符的 **L(bit53) 与 D(bit54) 互斥**：L=1 时 D 必须为 0，同时置位加载 CS 就 #GP。写描述符常量时要把 G(bit55) 和 D/B(bit54) 拆开，别混在一个 "LIMIT_FLAT" 里。
- `boot/entry.S` 的 pt_regs 压栈顺序与 `src/traps.rs` 的 `struct PtRegs` 字段顺序必须逐项对应，改任何一侧都要同步改另一侧。`orig_rax` 那一格三用途（系统调用号 / 异常错误码 / `!irq`），沿袭原版 `orig_eax` 的做法。
- 系统调用参数用 `rdi/rsi/rdx/r10/r8/r9`（不是 `rcx`）：`syscall` 指令会无条件破坏 rcx(返回地址) 和 r11(rflags)，所以第四个参数必须避开 rcx。这样 `int 0x80` 和将来的 `syscall` 路径能共用同一个 `do_syscall`。
- **原版 `schedule()` 的候选扫描跳过 `init_task`**：循环是「先 `p = p->next_task` 再判是否回到 `&init_task`」，所以 idle 从不参与 counter 比较，只作兜底。但时间片重发那段 `for_each_task` **包括** init_task。这个不对称很容易漏，漏了就是新任务永远抢不到 CPU。
- `ret_from_sys_call` 只在**返回用户态**时检查 need_resched（原版有 `cmpw $KERNEL_CS,CS(%esp)` 守卫，在内核态返回路径上调度会破坏内核栈上的调用链）。所以纯内核态的任务（idle、内核线程）必须自己轮询 need_resched，不能指望中断返回路径。
- 8259A 需要重映射：原版在 `boot/setup.S` 里做，我们的 setup.S 没做（当时不需要中断），所以放在 `irq::init()`。标准 ICW1..ICW4 四步，每步之间要 `outb_p`（往 0x80 写一字节等总线）。EOI 要**先发再调处理函数**，同原版 BUILD_IRQ 宏。
- 原版 `task_struct` 近 70 个字段，多数依赖未移植的 fs/signal/mmap。移植时只保留调度器自身需要的，并在每个字段注释里写上原版名字，后续补齐时有据可查。同理原版 `sys_call_table` 137 项，用「默认 ni_syscall + 显式覆盖」建表而不是一字排开 130 行。
- 原版 `__sleep_on` 把 `struct wait_queue` 放在**调用者栈上**再挂进链表。Rust 里这是明目张膿的悬垂引用风险，改成侵入式的 task 下标链（链接字段单独放一个 `WAIT_NEXT` 数组）。
- 调用号用的是 **x86_64 正式表**（`arch/x86/entry/syscalls/syscall_64.tbl`），不是 1.0.9 的 i386 号。`src/syscall/mod.rs` 的 `nr` 模块按号顺序排满 0..=334，之后只补 io_uring(425-427)/pidfd_open(434)/clone3(435)/faccessat2(439)/epoll_pwait2(441)。自检专用的私有号在 500 段（`IDLE=500`、`UNUSED=501`）。加新调用号一律照正式表填，不要按功能分组手写——那样必然重号，见 buglog bug-026。
- 分发表现在是「按号顺序逐项赋值 + 一张 `WIRED` 位图」。位图存在的理由：release 下 LLVM 会把函数体相同的 `sys_*` 折叠成同一地址，靠比函数指针数已实现槽位会漏数，见 buglog bug-028。
- `ni_syscall` 返回 `-EINVAL`（号合法但未实现，照抄 1.0.9），`do_syscall` 的越界分支返回 `-ENOSYS`。两条路径的返回值不同，自检靠这个区分，别统一。

- **selftest 里用 `serial::print`/`serial::print_dec` 而不是 `kprintln!`**（2026-08-07）：`kprintln!` 宏展开成 `println!` + `sprintln!`，每次调用生成**两份** `format_args!` 描述符（静态 `.rodata`：格式片段数组 + 参数类型数组）。opt-level=0 下 LLVM 不合并不同调用点的相同字面量，50 个 `kprintln!("ext4: {} -> ok", tag)` 产生 ~100 份描述符，把 selftest 对象从 8KB 涨到 43KB，直接撞 `_kernel_end <= 0x90000` 的 ASSERT。修法：selftest 里改用 `crate::serial::print(s: &str)` 和 `crate::serial::print_dec(v: u64)` 直接写串口，**零 `format_args!`**；失败才打印，成功静默；宏 `check!(tag, bool_expr)` 在检查宏里做分支。降到 8KB（减少 80%）。

- **`next_level` 必须对已存在的 PRESENT 条目检查并追加 USER 位**（2026-08-07）：`next_level` 原来在条目已 PRESENT 时直接返回物理地址而不检查 USER。clone_kernel_pdpt 从启动 PML4 拷贝条目（无 USER），后续 map_page 调 next_level 时条目已 PRESENT → 走快速返回 → USER 从未写入 → CPU 在 ring-3 访问时发现某一级缺 U/S 位 → #PF(0x5)。修复：`if user && (e & USER == 0) { set_entry(table, idx, e | USER); }` 写在 PRESENT 分支里。见 bug-034。

- **用户→内核切换时必须恢复 CR3**（2026-08-07）：`switch_to_task` 仅 `next_cr3 != 0` 时才写 CR3 → 用户进程(pml4≠0)切到内核线程(pml4==0)时 CR3 不切换 → 内核线程跑在用户 PML4 上。release() 里的 free_page(pml4) 之后 CR3 指向已回收页 → TLB 缺失读到零 → PF。修复：next_cr3==0 时切回启动页表(0x4000)；pml4 的 free 移到 release()（父进程上下文，不在 do_exit 里提前 free）。见 bug-035。

- **`sys_exit` 必须走 `do_exit` 的完整路径**（2026-08-07）：sys_exit 自己 inline 了 Zombie+schedule，漏掉 notify_parent → 父进程 wait4 永远睡。任何想退出任务的地方都必须经过统一的 do_exit。见 bug-036。

- **`get_page_flags` 只看叶子 PTE 的 flags，中间级（PML4/PDPT/PD）的 USER 位由 `next_level` 逐级保证**（2026-08-07）：不要用 get_page_flags 的返回值判定「全路径 USER 正确」，它只返回叶子标志。真正的全路径检查要逐级 `entry()` 读每个中间表的对应条目。见 bug-034 的诊断过程。

- **用户态自检的退出码避开 1..=31**（2026-08-07）：encode_status 把 [1,31] 的 code 当信号号处理（exit(5) → status=0x5="SIGTRAP"），正常退出码 1..=31 会被误导。用户自检用 exit(42) 临时绕过，等「do_exit(signal) vs exit(code)」的区分解。见 bug-037。

- **`block_bitmap_hi` 是 offset 32 的 little-endian u32，不是 offset 34 的字节**（2026-08-07）：`group_desc.rs::rd32(32)` 读 `u32::from_le_bytes([d[32],d[33],d[34],d[35]])`。要让 `hi=1`，必须 `d[32]=1`（LSB 在低地址），不是 `d[34]=1`（那会让 rd32(32)=0x00010000，hi 高 16 位为 1 而非低 16 位）。见 bug-030。规律：**构造 LE 字段的测试数据时，永远把期望值写进最低地址字节（小端序 LSB first）**。

- **信号位号约定：位号 == 信号号，bit 0 空着**（2026-08-07）：原版 `signal`/`blocked` 是 32 位字装 31 个信号，必须 `1 << (sig-1)`；我们是 64 位，统一成 `1 << sig`，`do_signal` 里 `trailing_zeros()` 拿到的直接就是信号号。**新增任何碰 signal/blocked 的代码都照这个约定**，别按原版 C 抄 `sig-1`——两套并存过一次，`blocked` 整体错开 1 位、屏蔽字形同虚设（屏蔽 SIGTERM 实际屏蔽 SIGSTKFLT），见 bug-032。约定写在 `Signal::mask()` 的文档注释里。

- **`send_sig` 必须照抄原版 `generate()` 的置位前过滤**（2026-08-07）：SIG_DFL 且 ∈{SIGCHLD,SIGCONT,SIGWINCH} → **不置位**；SIG_IGN 且非 SIGCHLD → 不置位。这不是优化而是语义前提：`sys_wait4` 睡在 Interruptible 上，醒来看到任何未屏蔽待处理信号就返回 `-EINTR`，而 `notify_parent` 发的正是 SIGCHLD——置了位父进程就被自己等的孩子打断，永远收不到尸。唤醒走独立的 `wake_up_waiter`，不置位照样叫醒（同原版 `send_sig` + `wake_up_interruptible` 分开调）。见 bug-031。

- **`_kernel_end <= 0x90000` 是硬约束，别按任务铺静态数组**（2026-08-07）：`SigAction` 32 字节 × 32 信号 × `NR_TASKS` 16 = 16KB BSS，一口气吃掉全部余量（余量从 23KB 掉到 2.8KB）。原版 `task_struct` 本身占一页、表内联在里面，**没有**独立静态数组，照抄成 `[[T; N]; NR_TASKS]` 是误读。修法：只存页地址 `[usize; NR_TASKS]`（128 字节），首次写才 `get_free_page`，`release` 时 `free_page`——绝大多数任务从生到死不调 `sigaction()`。见 bug-033。**规律：任何 per-task 的大结构（sigaction/filp/rlim）都走按需分页，不进 BSS。**

- **内核态 fork 出来的子进程不能照原样 iret 回去**（2026-08-07）：`sys_fork` 复制的 pt_regs 里 `rsp` 指向**父进程**的内核栈（`int 0x80` 时的 rsp）。用户态 fork 没这问题（父子各有自己的用户栈，页表 COW 分开），内核态 fork 则会让父子在同一个栈上跑同一段代码，必然互踩。所以内核上下文测 fork 必须换掉子进程的返回现场（改 pt_regs 的 `rip`/`rsp`），这正是 execve 之后要做的事。见 `exit::fork_selftest`。

- **`ret_from_sys_call` 的 do_signal 钩子对内核态返回是故意跳过的**：钩子放在 `cmpw $KERNEL_CS, PT_CS(%rsp); je restore_and_iret` **之后**（同原版），所以 `PT_CS == KERNEL_CS` 的返回不投递信号。推论：**内核上下文的自检永远走不到 entry.S 那个钩子**，只能直接调 `signal::do_signal` 测决策逻辑；钩子本身要等 execve/用户态落地才能端到端验证。

- **汇编里不要写死 Rust 结构体的字段偏移**：`ret_from_sys_call` 判「有没有待处理信号」时，原版直接在汇编里读 `task_struct` 的 `signal`/`blocked` 偏移。我们不能照抄——`current` 是个**下标**（`sched::CURRENT`）不是指针，且 `Task` 的字段偏移由 Rust 布局决定，写死会随字段增删静默错位。改成 `call signal_pending_c`（`#[unsafe(no_mangle)] extern "C" -> u8`），常态路径多一次 call，换来偏移不会失同步。

- **声卡子系统移植完成但 debug 构建偏大**（2026-08-07）：新增 ~2700 行代码导致 debug 镜像超 0x91000。将 sound + hd 模块放到 `extra-drivers` feature gate 后，release 构建 LTO 可容纳全部功能。大型可选子系统应走 feature gate 而非默认编译。

- **不要在内核代码中放大静态查找表**（2026-08-07）：`ULAW_DSP`(512B) + `DSP_ULAW`(256B) + `MIDI_NOTE_FREQ`(512B) = 1280B rodata，对内核镜像的 0x91000 上限是昂贵的。μ-law 编解码和 MIDI 频率计算可用 G.711 标准算法和整数近似代替。

- **edition 2024 注意事项**（2026-08-07）：`asm!("outb %al, $0x80")` 报 "unknown token"，改用 `PortWriteOnly::new(0x80).write(0u8)`。`#[no_mangle]` 需改为 `#[unsafe(no_mangle)]`。`f64::powf` 在 `#![no_std]` 中不可用。

## Do-Not-Repeat

<!-- Mistakes made and corrected. Each entry prevents the same mistake recurring. -->
<!-- Format: [YYYY-MM-DD] Description of what went wrong and what to do instead. -->
- [2026-08-06] **低 1GB 是恒等映射，所以空指针读不会 page fault——它静默返回实模式 IVT 的字节。** 任何地方看到 `0xff53`、`nlink=255`、`0xf000ff53`，先怀疑「某个指针/基址是 0」或「读到了缓冲上一轮的内容」，不要当成内存被写坏。F000:FF53 是 BIOS 的 IRET 桩，IVT 里大量未用向量都指向它，所以从地址 0 读一整块出来就是这个重复模式。判据：坏值的每个字节都能在 IVT 对应偏移上找到。
- [2026-08-06] **别把「屏障」当成 `noalias`/`readonly` 的解药。** 缓冲数据区是驱动用裸指针 memcpy 填的，把它当 `&[u8]` 交出去，LLVM 就能证明「切片存活期间无人写」并沿用上一轮的载入。`compiler_fence` 无效（只约束原子操作），空 `asm!` 的 memory clobber 也无效（屏障之后新建的引用照样满足 readonly 推断）。有效的两种：(a) 根本不建引用，全程 `read_volatile`；(b) 把指针本身穿一条 `asm!("/* {0} */", inout(reg) p)` 再建切片，断掉来源推断。见 bug-029。
- [2026-08-06] **失败率对代码生成敏感时，停止「读源码找错处」。** 本轮只改注释/删诊断打印就让失败率在 24%↔56% 之间跳；同一手 launder 加到 `data()` 上把 44% 压到 24%，加到 `inode()`/`sb()`/`buf_ptr()` 上反而升到 64%。这种敏感性排除了「某一处源码写错」，指向栈帧尺寸/布局相关的破坏或与中断时序耦合的竞态。该换手段（栈护栏高水位、单步比对反汇编、把可疑路径整段串行化），不要继续加屏障或改访问器。
- [2026-08-06] **测量前必须确认镜像真的重建了。** `scripts/build.sh` 编译失败时 `test.sh` 提前退出、`serial.log` 保持上一轮不变，于是 25 次循环全读同一份旧日志，报出 25/25 全过的假结果。每次循环前 `rm -f target/boot/serial.log`，并且先单独 `cargo build` 确认 0 error 再进循环。
- [2026-08-06] **`git checkout <file>` 会连本会话未提交的诊断脚手架一起丢掉。** 想回退一处改动时对整个文件 checkout，把同文件里的 `INODE_AREA` 护栏一起revert了。回退单点改动用 Edit 精确改回，不要整文件 checkout。
- [2026-08-06] **静态数组一旦大到 KB 级必须评估 BSS 余量，否则等链接期 ASSERT 拦下就太晚了。** `KSTACKS` 48KB 让 `_kernel_end` 从 0x854F0 冲到 0x922C0，超 0x90000 共 8896 字节。BSS 余量只有约 44KB（0x90000 - 内核镜像末尾），加一个大数组前要先 `nm target/boot/system.elf | grep _kernel_end` 确认还有空间，或者直接按 `page_ref` (bug-024) 和本次 `KSTACKS` 的做法改成动态划分。
- [2026-08-06] **「换 ext4 就能绕开」是个要核实的假设，不能直接写进 Next phase。** 这次说「minix 的 flake 换 ext4 就好」，读代码后发现：(1) `do_unlink`/`dir_namei`/buffer cache 都在 VFS 层，换 ext4 会一起带过去；(2) ext4 模块只有 1291 行结构定义，没有 `read_inode`/`lookup`/目录操作/块分配/mkfs，`FsType::Ext2` 除了定义和一句 `TODO` 全树没人用，无法挂载。「换 ext4」不是配置开关，是个与刚做完的系统调用工作量相当的独立模块。假设涉及代码能力时，先 grep/读关键函数确认再写结论。
- [2026-08-02] 用 `if=floppy` 手工启动镜像 → 根本没引导，截屏只有 BIOS 的 "no bootable device"。手工跑 QEMU 时要照抄 `scripts/test.sh` 里的 `QEMU_BASE`（`if=ide`）。
- [2026-08-02] `screendump` 紧跟启动就发 → 抓到的是内核跑之前的空屏。必须先 `sleep` 几秒等启动完成。
- [2026-08-02] 把 E820 的 usable 区间原样灌进空闲页链表 → 第一次 `get_free_page` 就派出 0x70000 的页表，清零时三重错误。任何物理内存分配器都必须先保留低 1MB。详见 buglog bug-001。
- [2026-08-02] `mem_map` 表体紧贴内核镜像（0x48000）向后放会盖住 0x70000 的页表；表基址要 `.max(0x100000)`。
- [2026-08-06] **“每个物理页一个条目”的表绝不能写成定长静态数组。** `page_ref.rs` 按最大内存开 `[AtomicU32; 65536]` = 256KB BSS，`_kernel_end` 冲到 0xCC2B0，被 `kernel.ld` 的 ASSERT 拦下。这类表要照 `mem_map` 的做法：在 `page_alloc::init` 里按实际物理内存量划出来、把 `map_end` 抬到表尾之后，再 `attach()` 给模块。这是 BSS 越过 0x90000 的第三次事故（前两次：BSS 盖页表、kstack 池）。详见 buglog bug-024。
- [2026-08-06] **别把一次失败硬塞进已有的 buglog 条目。** 把 `SUPER_AREA` 护栏 panic 记成「bug-023 的既有 flaky」，用户直接指出护栏一直是 100% 正常的。bug-023 的症状清单里根本没有护栏 panic。归因到既有 bug 之前，先核对症状是否真的对得上，对不上就单独立条（这次是 bug-025）。
- [2026-08-06] **检查器自己不能建覆盖被检内存的引用。** `check_guards` 用 `&*addr_of!(SUPER_AREA)` 建共享引用去查护栏，与 fs 路径上存活的 `&mut SuperBlock` 重叠即 UB，比较被折叠 → 报「护栏破了」而打印出来的值恰好是正确的护栏。判据很好记：**一次 `read_volatile` 的结果同时喂比较和打印，若打印值是对的而比较说不等，那就是比较错了，不是内存坏了。** 检查器要么全程裸指针，要么失配后复读确认再 panic。见 bug-025。
- [2026-08-06] 位域标记别和计数字段重叠：`page_ref` 的 COW 标记原本用 `0x8000`，而计数是 `& 0xFFFF`，引用计数到 32768 就被误读成 COW 页。标记位要挪到计数字段之外（bit 31）。

- [2026-08-02] 把原版 `number()` 的 `while(size-->0)` 直译成 `if size>0 { push_n(size) }` → 右对齐补位翻倍（`%8d` 输出 16 列）。C 版的自减会消耗掉 `size`，让第二段补位分支空转；Rust 版补完必须显式 `size = 0`。详见 buglog bug-002。
- [2026-08-02] 写自检断言前先确认原版语义：`number()` 的 `SIGN` 标志对正数**不**产生 `'+'`，只有配合 `PLUS`（或 `SPACE` 出空格）才有符号。断言写错会误报实现有 bug。

- [2026-08-02] **`asm!` 的 `options(nomem, nostack)` 对会触发异常/中断的指令是谎报**。`nomem` → 处理函数改的全局变量被 CSE 掉；`nostack` → 局部变量被放到 rsp 以下，而处理函数会在当前栈上压 pt_regs 并跑完整个 printk 把它们覆盖。`int3`/`div`/`ud2`/`int 0x80` 这类指令一律不加这两个选项。详见 buglog bug-004。
- [2026-08-02] **只在中断/异常里被改的全局变量，读取侧必须 `read_volatile`**。`jiffies`（时钟中断自增）、`TRAP_COUNT`（异常处理自增）、跨任务共享的计数器都是。原版 `sched.h` 把 jiffies 声明成 `unsigned long volatile` 就是同一个用意。忘了就是等待循环变死循环。详见 bug-005/006。
- [2026-08-02] 自检里「探针前后读计数器求差」的比较逻辑要抽成 `#[inline(never)]` 函数。内联时 LLVM 会把「读—比较」对下沉复制到每个使用点，表现为三个 bool 各自打印 true 但 `&&` 结果为 false。
- [2026-08-02] 给 `do_timer` 加 `if current_nr() != 0` 想让 idle「不参与时间片核算」→ idle 的 counter 永不归零、need_resched 永不置位、永不让出 CPU。原版对 current 无条件递减。详见 bug-008。
- [2026-08-02] 调度自检里让测试任务只 `yield` 不 `sleep` → 它们互相乒乓，task[0]（idle）再也拿不到 CPU，主自检挂死。这是原版调度语义（idle 只在无其他 Running 任务时被选），不是 bug；测试任务必须真的睡。
- [2026-08-02] **看到 `test.sh` 输出里有 `ERROR:`/`terminating on signal 15` 不要直接当失败**。先看最后一行的 `[PASS]/[FAIL]`：`ld.so.preload` 那行是 snap-confine 的 AppArmor 环境噪音（详见 bug-010，已在 build.sh 过滤）；`signal 15 from timeout` 是设计如此——内核末尾 `idle_loop()` 里 `hlt` 空转，正常退出码就是 124。
- [2026-08-06] 手写系统调用号时按「进程管理/文件操作/信号…」分组编号 → 出现 `GETPID`=`WAIT4`=61、`GETPPID`=`KILL`=62、六个 `IO_*` 全是 0（把 `t[READ]` 也覆盖了）。建表是 `t[nr::X] = ...` 的顺序赋值，重号会**静默覆盖**，编译器不报任何警告，自检才发现 getppid 实际跑的是 sys_kill。调用号只能照正式表逐号填。详见 buglog bug-026。
- [2026-08-06] 改完代码跑 `scripts/test.sh`，结果和改动完全不符 → 工作区里有未提交的本地改动把 `bash scripts/build.sh` 注释掉了，测的是旧镜像。**自检结果异常时先确认 `serial.log` 里出现了本次改动引入的新字样**（比如改过的打印文案），再去怀疑代码。详见 buglog bug-027。
- [2026-08-06] 用 `f as usize != ni_syscall as usize` 数表里已实现的项 → release 下少数一个。identical code folding 会合并函数体相同的实现，Rust 也不保证 fn 指针相等的语义。要数「有没有被赋值」就另建 bool 位图。详见 buglog bug-028。
- [2026-08-06] 顺手用了 `git stash` 想拿基线对比 → 把本次未提交的工作全卷走了（`git stash pop` 救回）。这个树的改动都没提交，**不要用 stash 做基线对比**，改用 `git worktree` 或直接看已有的历史记录数据。

### 2026-08-06 — 中断入口在 SAVE_ALL 之前动寄存器（bug-029 根因）
**别在 SAVE_ALL 之前碰任何通用寄存器。** `irq_common` 原本 `popq %rax` 取 IRQ 号，
把被打断上下文的 %rax 毁在保存之前，SAVE_ALL 存的是 IRQ 号，RESTORE_ALL 如实恢复
——每个滴答都让被打断的内核代码带着 %rax = 0 继续跑。要传向量号就用 CPU 已经压好的
那一格（orig_rax 存 ~nr），或者先 SAVE_ALL 再从 pt_regs 上方的临时槽取。

**诊断顺序上的教训（这条比修复本身值钱）：**
- 「失败率对无关代码改动敏感」= 寄存器分配/活跃区间敏感 = **某个寄存器被外力改了**。
  这不是「LLVM 优化问题」也不是「内存破坏」，我在这两条错路上花了整整两个会话。
- 一个 `cli` 包住可疑区段的实验（20/20 过 vs 9/20）在 5 分钟内把范围从「整个 fs 层」
  缩到「中断路径」。**先做能一刀切掉半个假设空间的实验，再读源码。**
- 加屏障（`compiler_fence`、空 `asm!` memory clobber）、改访问器、调栈大小——全是在
  给一个寄存器 bug 打内存补丁。症状「像」内存问题不代表它是。

## Decision Log

<!-- Significant technical decisions with rationale. Why X was chosen over Y. -->
- [2026-08-02] **不引入 `bootloader_api`**，尽管用户最初要求。原因：该 crate 的 `entry_point!` 依赖 `bootloader` crate 把内核当 ELF 加载并传入 `BootInfo`，而本项目有自建引导链且内核是 staticlib；且它提供的是线性 framebuffer，拿不到 0xB8000 文本模式。保留现有 `extern "C" start_kernel(*const BootParams)` ABI，它承担同样职责。
- [2026-08-02] 控制台走 `fmt::Write` + `format_args!`，而不是继续手写 `print_dec` 之类的辅助函数：能直接用 `{}`/`{:#04x}` 格式化，且不需要堆分配。`serial.rs` 暂时保留 `print_dec`，因为测试脚本解析的串口输出格式不宜变动。
- [2026-08-02] 颜色属性存在 `Writer` 里而非每次调用传参；`cprint!` 通过「存旧值→输出→恢复」实现，这样 `print!` 保持简洁，panic handler 又能整段染色。
- [2026-08-02] **上 nightly + 用 `x86_64` crate**（用户拍板）。之前悬而未决的两个问题就此关闭，不再手写 IDT/PIC 结构体。
- [2026-08-02] MM 只保留原版真正的机制（`mem_map` 引用计数、空闲链表指针存在空闲页内、kmalloc 的 order 分档 + 魔数校验、整页空闲则归还），跳过原版为 386 时代所做的妥协（`secondary_page_list` 备用池、`wp_works_ok` 探测、`try_to_free_page` 换页）。后两者要等块设备和进程到位。
- [2026-08-02] `kmalloc` 的 `block_header` 不用裸 union（原版用 union 省 4 字节），改成两个字段，头从 8B 变 24B，档位大小按 64 位重算成 32/64/128/256/512/1024/2048/4080。可读性优先于省几字节。
- [2026-08-02] `unmap_page` 只清 PTE，不回收中间级页表（PD/PT）——与原版一致。自检里那 2 页差值是预期占用，已在输出里注明 `expect 2` 免得被当成泄漏。
- [2026-08-02] **`vsprintf` 收 `core::fmt::Arguments` 而不是 `%d` 格式串**。原版靠 `va_list` 解析 `%` 转换符，Rust 没有等价物；自己实现一遍等于放弃类型安全。保留原版 `number()` 的补位/进制/符号语义（printk 的 `%08lx` 对齐还要用），格式串解析交给 `core::fmt`。
- [2026-08-02] **`klib::string` 全部在裸指针上工作**而不是 `&str`/`&[u8]`。理由：内核之后要处理的就是 C 字符串（execve 的 argv、文件名、/proc 输出），用 `&str` 会在每个边界上来回转换。Rust 侧便利封装单独给 `c_str()` / `c_str_bytes()`。
- [2026-08-02] `Cursor` 的 `len()` 返回「本该写多少」而非「实际写了多少」，与 C 的 `snprintf` 一致；截断通过 `truncated()` 判断。这样调用方能算出需要多大缓冲。
- [2026-08-02] errno 常量给成**正值** `i32`，调用方写 `-EINVAL`，和原版 C 代码一致；同时提供 `KResult<T>` + `from_raw`/`to_raw` 供 Rust 侧用 `?`。用户态那个全局 `errno`（`lib/errno.c`）不移植，内核自身从不读它。
- [2026-08-02] **异常处理合成一个 `do_trap` + `TRAP_INFO` 表**，而不是照原版 `DO_ERROR` 宏展开 14 份几乎相同的 `do_xxx`。语义等价（每份原版实现都是「记 trap_no/error_code → send_sig → die_if_kernel」），少 14 份重复代码。向量元信息（name/signr/has_error_code）逐条照抄原版的宏参数。
- [2026-08-02] **IRQ 只生成一份汇编桩**，不照原版 `BUILD_IRQ` 生成 完整/fast/bad 三套。原版靠 `sa_flags` 的 `SA_INTERRUPT` 在装门时选桩，我们把这个区分挪到 Rust 侧 `do_IRQ` 里判断 `action.fast`。热路径多一个分支，换来少维护两套汇编；且目前没有需要 SA_INTERRUPT 的驱动。
- [2026-08-02] **task 表用定长数组 + 下标**而不是原版的裸指针环。Rust 里自引用结构要么 Pin+unsafe 要么用下标；`NR_TASKS` 本来就是定长，下标更清晰。环的语义完整保留（`Task::next` 仍构成环，`schedule` 仍是遍历环）。
- [2026-08-02] **加了原版没有的 `kernel_thread()`**。原版 1.0.9 的 init 是 `sys_fork()` + `execve()` 出来的用户态进程，内核里没有「只跑内核代码的任务」这个概念。但 `sys_fork` 的完整语义要 `copy_page_tables`（依赖尚未移植的 `mm/mmap.c`），而调度器现在就该验证——内核线程共用内核页表，绕开整个 VM 复制问题。
- [2026-08-02] **`vsprintf` 之后，异常/中断里的打印统一走 `klib::printk` 的 `pr_*!` 宏**而不是 `kprintln!`：环形缓冲能在 panic 后回放，且 `console_loglevel` 可以在噪声大的路径上调低（`die_if_kernel` 照原版 `console_verbose()` 把它提到 15）。
- [2026-08-02] `syscall` 指令入口（`syscall_entry`）留了符号但**暂不启用**：它进入时不换栈，需要 per-cpu 的用户栈暂存位置。目前只走 `int 0x80`（原版唯一的路径）。

- [2026-08-02] **凡是「可能会睡」的子系统都不能在 task[0] 里跑**。fs/buffer 的每个入口（getblk/wait_on_buffer/iget/bread）契约都写着会睡，而 `sleep_on` 对 task[0] 直接 panic。在 task[0] 里跑 mount_root 缓冲够用时能侥幸通过，一旦触到竞态分支就随机丢块、目录项 inode 号损坏。原版的 mount_root 在 init 进程（task[1]）里。要跑就起内核线程。详见 buglog bug-012。
- [2026-08-02] **写缓冲只覆盖一部分时必须先 `bread` 而不是 `getblk`**。getblk 给的是刚回收的缓冲，未覆盖的字节是上一个块的残留；置 b_uptodate 再 sync 就把残留写回盘上。这条对自检代码同样成立——我自己在自检里踩了。详见 buglog bug-013。
- [2026-08-02] 内核镜像 BSS 增长会静默盖掉 setup.S 在 0x70000 建的页表（head.S 清 BSS 时清掉页表 → 三重错误、串口全空）。加模块前先看 `_kernel_end`。已把页表搬到 0x4000。详见 buglog bug-011。
- [2026-08-02] 诊断间歇性 bug 要建「收支账本」而不是逐点加日志：给 alloc/free 各打一行，再用脚本对账，能一步定位到「块 21 分配了从未释放」，比猜哪个分支错快得多。
- [2026-08-02] 自检里的断言要用**精确相等**而不是 `>=`：`free_after >= free_before` 会把「只回收了一部分」判成通过。基线要在任何分配之前取。

- [2026-08-02] **移植睡眠代码时，`cli()`/`sti()` 的位置和条件判断的顺序都是语义的一部分，不是噪音**。原版 `make_request` 带着关中断状态调 `sleep_on`；原版 `__wait_on_buffer` 先 `add_wait_queue` 再在循环里判条件。两处都是为了消掉「判完条件 → 挂上队列」之间的丢失唤醒窗口。照抄结构而漏掉顺序，症状是低概率的任务永久睡死（缓冲 b_count 卡住 → truncate 漏块）。详见 buglog bug-016/017。
- [2026-08-02] **不要在同一个表达式里通过同一个 `static mut` 访问器取 `&mut` 字段和读另一个字段**。`(*addr_of_mut!(bh(n).b_wait)).sleep_on_while(|| bh(n).b_lock)` 是重叠可变借用，优化后闭包读到过期值，症状是直接三重错误。先把 `&mut *bh(n)` 降成裸指针，再分别取各字段指针，条件用 `read_volatile`。详见 buglog bug-018。
- [2026-08-02] **panic handler 必须走串口**（`kprintln!` 而非 `println!`）。无头测试只能看串口，panic 信息只上 VGA 等于没有诊断。改完之后连续几个 bug 都是一行日志定位。详见 buglog bug-019。
- [2026-08-02] x86_64 的内核栈需求比原版 i386 大得多（指针 8 字节、寄存器多一倍）。原版一页 `kernel_stack_page` 在这里不够跑 fs 调用链；栈上别放 KB 级数组。跑完显式查一次栈底魔数，比从 page fault 的 CR2 反推快。详见 buglog bug-014。
- [2026-08-02] 链接脚本里加 `ASSERT` 拦住布局事故（`_kernel_end <= 0x90000`）。BSS 盖掉 setup.S 留下的参数区/E820 表已经发生过两次，加断言后是链接期报错而不是运行期玄学崩溃。加完记得故意改小阈值验证它真的会触发。详见 buglog bug-015。
- [2026-08-02] 诊断顺序上，**先修「让症状可见」的东西**（panic 上串口、给 data()/inode() 加边界断言、给 mkfs 加写后读校验），再去查根因。加断言把「随机位置的 page fault」变成「确定性的一行信息」，比多跑 50 次 QEMU 便宜。

- [2026-08-02] **原版每一处 `cli()`/`sti()` 都要照搬，包括它罩住的范围**。`add_request` 的 cli 一直罩到 `request_fn()` 调用完；漏掉之后 end_request（中断上下文）与它交错，请求的 bh 指针错位，读块 1 会拿到别的块的内容并置上 b_uptodate。移植时看到 cli/sti 先问「它防的是谁」，再决定边界放哪。详见 buglog bug-021。
- [2026-08-02] **`PanicInfo::message().as_str()` 对带格式参数的 assert 返回 None**。`assert!(c, "idx={}", i)` 的消息拿不到，日志里只剩 file:line，精心写的诊断值全丢。直接 Display `info.message()`。详见 buglog bug-022。
- [2026-08-02] 缓冲/块设备层的随机损坏，先加**不变量护栏**再查根因：`bh()`/`inode()` 的下标断言、`data()` 的空指针检查、链表下标的范围断言、数据页必须 >= 1MB、mkfs 写后读校验、「一块一缓冲」检查。这些把「随机位置的 page fault」变成「一行带数值的断言」，是后续每一步定位的前提。
- [2026-08-02] 看到内存里出现 `0xf000ff53`（或 `f000:e2c3` 之类）要立刻想到**BIOS ROM**（0xF0000 段的 IRET stub），说明某个指针落到了低端保留内存，而不是数据本身出错。
