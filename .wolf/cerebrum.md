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

## Do-Not-Repeat

<!-- Mistakes made and corrected. Each entry prevents the same mistake recurring. -->
<!-- Format: [YYYY-MM-DD] Description of what went wrong and what to do instead. -->
- [2026-08-02] 用 `if=floppy` 手工启动镜像 → 根本没引导，截屏只有 BIOS 的 "no bootable device"。手工跑 QEMU 时要照抄 `scripts/test.sh` 里的 `QEMU_BASE`（`if=ide`）。
- [2026-08-02] `screendump` 紧跟启动就发 → 抓到的是内核跑之前的空屏。必须先 `sleep` 几秒等启动完成。
- [2026-08-02] 把 E820 的 usable 区间原样灌进空闲页链表 → 第一次 `get_free_page` 就派出 0x70000 的页表，清零时三重错误。任何物理内存分配器都必须先保留低 1MB。详见 buglog bug-001。
- [2026-08-02] `mem_map` 表体紧贴内核镜像（0x48000）向后放会盖住 0x70000 的页表；表基址要 `.max(0x100000)`。

- [2026-08-02] 把原版 `number()` 的 `while(size-->0)` 直译成 `if size>0 { push_n(size) }` → 右对齐补位翻倍（`%8d` 输出 16 列）。C 版的自减会消耗掉 `size`，让第二段补位分支空转；Rust 版补完必须显式 `size = 0`。详见 buglog bug-002。
- [2026-08-02] 写自检断言前先确认原版语义：`number()` 的 `SIGN` 标志对正数**不**产生 `'+'`，只有配合 `PLUS`（或 `SPACE` 出空格）才有符号。断言写错会误报实现有 bug。

- [2026-08-02] **`asm!` 的 `options(nomem, nostack)` 对会触发异常/中断的指令是谎报**。`nomem` → 处理函数改的全局变量被 CSE 掉；`nostack` → 局部变量被放到 rsp 以下，而处理函数会在当前栈上压 pt_regs 并跑完整个 printk 把它们覆盖。`int3`/`div`/`ud2`/`int 0x80` 这类指令一律不加这两个选项。详见 buglog bug-004。
- [2026-08-02] **只在中断/异常里被改的全局变量，读取侧必须 `read_volatile`**。`jiffies`（时钟中断自增）、`TRAP_COUNT`（异常处理自增）、跨任务共享的计数器都是。原版 `sched.h` 把 jiffies 声明成 `unsigned long volatile` 就是同一个用意。忘了就是等待循环变死循环。详见 bug-005/006。
- [2026-08-02] 自检里「探针前后读计数器求差」的比较逻辑要抽成 `#[inline(never)]` 函数。内联时 LLVM 会把「读—比较」对下沉复制到每个使用点，表现为三个 bool 各自打印 true 但 `&&` 结果为 false。
- [2026-08-02] 给 `do_timer` 加 `if current_nr() != 0` 想让 idle「不参与时间片核算」→ idle 的 counter 永不归零、need_resched 永不置位、永不让出 CPU。原版对 current 无条件递减。详见 bug-008。
- [2026-08-02] 调度自检里让测试任务只 `yield` 不 `sleep` → 它们互相乒乓，task[0]（idle）再也拿不到 CPU，主自检挂死。这是原版调度语义（idle 只在无其他 Running 任务时被选），不是 bug；测试任务必须真的睡。
- [2026-08-02] **看到 `test.sh` 输出里有 `ERROR:`/`terminating on signal 15` 不要直接当失败**。先看最后一行的 `[PASS]/[FAIL]`：`ld.so.preload` 那行是 snap-confine 的 AppArmor 环境噪音（详见 bug-010，已在 build.sh 过滤）；`signal 15 from timeout` 是设计如此——内核末尾 `idle_loop()` 里 `hlt` 空转，正常退出码就是 124。

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
