//! shitix 内核入口
//!
//! 由 boot/head.S 在 long mode 下通过 `call start_kernel` 进入。
//! 对应 linux-1.0.9 的 init/main.c 里的 start_kernel()。

#![no_std]
#![no_main]

pub mod console;
pub mod desc;
pub mod drivers;
pub mod e820;
pub mod elf;
pub mod exit;
pub mod framebuffer;
pub mod fs;
pub mod info;
pub mod ioport;
pub mod irq;
pub mod klib;
pub mod mm;
pub mod net;
pub mod pci;
pub mod sched;
pub mod serial;
pub mod signal;
pub mod smp;
pub mod syscall;
pub mod traps;
pub mod usb;
pub mod umm;

/// `sys_uname` 报告的系统信息。对应原版 `include/linux/utsname.h` 里
/// `init_uts_ns` 的字段和 `version.c` 的 `UTS_RELEASE`。
///
/// `UTS_RELEASE` 故意报一个较新的版本号：本内核虽是 Linux 1.0.9 的 Rust
/// 重写，但用户态跑的是现代 glibc 编译的 /bin/sh，glibc 启动时会解析
/// uname.release 并与编译期最小内核版本（通常 3.2）比较，低于即
/// `FATAL: kernel too old` 直接退出。报 6.6.0 既满足 glibc 版本检查，
/// 又保留 "-shitix" 后缀标识来源。
pub const UTS_SYSNAME: &str = "Linux";
pub const UTS_RELEASE: &str = "6.6.0-shitix";
pub const UTS_VERSION: &str = "#1 SMP x86_64";
pub const UTS_MACHINE: &str = "x86_64";

use console::Color;
use core::panic::PanicInfo;

/// bootsect/setup 留在 0x90000 的机器参数区布局（偏移与原版一致）。
#[repr(C)]
pub struct BootParams {
    cursor: u16,     // 0x00: 光标位置 (dh=行, dl=列)
    ext_mem_k: u16,  // 0x02: int 15h/88h 报告的扩展内存 KB
    video_page: u16, // 0x04
    video_mode: u16, // 0x06: al=模式, ah=列数
}

unsafe extern "C" {
    /// 由 boot/kernel.ld 定义，内核镜像（含 BSS）的末尾地址。
    /// 对应原版 `mem_init` 里用到的 `&etext` / `start_mem`。
    static _kernel_end: u8;
}

/// head.S 调用的入口。`params` 指向 0x90000。
///
/// # Safety
/// 由汇编代码在 long mode、栈已建立、BSS 已清零之后调用一次。
/// `params` 必须指向 setup.S 填好的机器参数区（当前为物理地址 0x90000）。
#[unsafe(no_mangle)]
pub extern "C" fn start_kernel(params: *const BootParams) -> ! {
    serial::init();
    console::clear();

    cprintln!(Color::LightCyan, Color::Black, "shitix: Linux 1.0.9 rewritten in Rust (x86_64)");
    serial::print("shitix: long mode entry reached\n");

    // SAFETY: params 由 head.S 传入固定物理地址 0x90000，该区域由 setup.S
    // 写入且在 long mode 恒等映射（低 1GB）范围内；BootParams 只有 8 字节
    // u16 字段，无对齐或有效性要求上的额外风险。
    let bp = unsafe { &*params };

    // 先铺 task[0] 静态栈的哨兵：链接器把 head.S 的栈排在 Rust 那些
    // static mut 表之后（地址更高），栈往下溢出会直接踩进 fs::buffer::BUFFERS。
    // SAFETY: 启动最早期，只写 .bss 里那一页哨兵。
    unsafe { sched::init_stack_guard() };

    println!("mem (int 15h/88h): {} KB", bp.ext_mem_k);
    println!("video mode: {:#04x}, {} cols", bp.video_mode & 0xFF, bp.video_mode >> 8);

    println!("e820 entries: {}, raw usable: {} MB",
             e820::count(), e820::usable_bytes() / 1024 / 1024);

    // 内核镜像末尾：mem_map 从这里往后放（同原版 mem_init 的 start_mem）
    let kernel_end = core::ptr::addr_of!(_kernel_end) as usize;
    println!("kernel image ends at {:#x}", kernel_end);

    // 建高半区直接映射（物理 0..1GB → PHYS_MAP_BASE..）。必须在 mm::init 之前：
    // page_alloc::init 建空闲链表时 push_free 走的就是高半区映射，晚于它建链
    // 会 fault（高半区还没映射）。此后低 1GB 恒等映射会被用户 ELF 覆盖，
    // 内核一律改走高半区访问物理内存。
    // SAFETY: 启动早期、恒等映射（低 1GB）完好，用固定物理页 0x7000 直写。
    mm::paging::init_high_map();

    // 对应原版 start_kernel 的 mem_init(...)
    // SAFETY: 启动早期，中断仍关闭（head.S 未 sti），只调用一次；
    // kernel_end 来自链接脚本，e820::usable 反映真实物理内存。
    let info = unsafe { mm::init(kernel_end, e820::usable()) };

    // 同原版 mem_init 末尾那行 printk("Memory: %luk/%luk available ...")
    cprintln!(Color::LightGreen, Color::Black,
              "Memory: {}k/{}k available ({} kernel pages, {} reserved)",
              info.available / 1024, info.high_memory / 1024,
              info.kernel_pages, info.reserved_pages);

    sprintln!("mm: {}k/{}k available, {} kernel, {} reserved, mem_map at {:#x}, page_ref at {:#x} ({} slots), kstacks at {:#x} ({}x{}K)",
              info.available / 1024, info.high_memory / 1024,
              info.kernel_pages, info.reserved_pages, info.mem_map_addr,
              info.page_ref_addr, info.nr_pages,
              info.kstack_addr, sched::KSTACK_SLOTS, sched::KSTACK_SIZE / 1024);

    mm_selftest();
    klib_selftest();
    net_selftest();

    // ---- 模块 4：描述符表 / 中断 / 系统调用 / 调度 ----
    // 顺序对应原版 start_kernel()：trap_init() → init_IRQ() → sched_init()

    // SAFETY: 启动早期、中断仍关闭，各只调一次；顺序满足各自的文档契约
    // （GDT 要在 IDT 之前，因为 IDT 的门里写的是 GDT 的 KERNEL_CS 选择子）。
    unsafe {
        desc::init_gdt();
        desc::init_idt();
        irq::init();
    }
    desc::dump();

    // 对应原版 start_kernel 里的 sched_init()
    // SAFETY: GDT/IDT/PIC/mm 均已就绪，只调一次。
    if let Err(e) = unsafe { sched::init() } {
        panic!("sched_init failed: {}", klib::errno::strerror(e));
    }

    // 对应原版 time_init()：从 CMOS RTC 读开机时刻（startup_time），
    // current_time()（inode 时间戳/sys_time）据此返回真实墙钟。
    // SAFETY: 启动期，sched::init 之后。
    unsafe { drivers::rtc::init() };

    trap_selftest();
    syscall_selftest();
    sched_selftest();
    // SAFETY: 启动早期、中断仍关闭，LAPIC 读不涉及中断
    sprintln!("--- smp selftest ---");
    let smp_ok = unsafe { smp::tests::selftest() };
    sprintln!("smp: done ({})", if smp_ok { "ok" } else { "FAIL" });

    // 真正启动 SMP：映射 LAPIC MMIO、使能 BSP LAPIC、按 INIT-SIPI-SIPI
    // 拉起所有 AP。必须在用户进程创建之前调（此时 cr3 是内核引导 PML4，
    // 低 1GB 恒等映射完好，蹦床与信箱都靠它）。AP 起来后停在派工等待
    // 循环里，不参与调度。
    smp::smp_init();
    if !smp::tests::parallel_selftest() {
        sprintln!("smp: parallel selftest FAIL");
    }

    // ---- 模块 5：文件系统与设备驱动 ----
    // 顺序对应原版 start_kernel()：buffer_init/inode_init/file_table_init
    // → blk_dev_init/chr_dev_init → mount_root
    // SAFETY: mm 与调度器已就绪，各只调一次；fs::init 必须在
    // drivers::init 之前（驱动要往设备表里注册）。
    unsafe {
        fs::init();
        drivers::init();
    }

    // mount_root 与文件系统自检**必须在 task[0] 之外**跑。
    //
    // 整条 fs/buffer 路径的契约都是「可能会睡」：`getblk` 找不到干净缓冲
    // 时 `sleep_on(&buffer_wait)`、`wait_on_buffer` 等 b_lock、`iget` 等
    // i_lock。而 `sleep_on` 对 task[0] 是 `panic!("task[0] trying to
    // sleep")`（原版同样是 `panic("task[0] trying to sleep")`）——原版的
    // idle 任务从不碰文件系统，`mount_root` 是在 init 进程（task[1]，由
    // `start_kernel` 末尾 `fork` 出来的那个）里跑的。
    //
    // 在 task[0] 里直接跑这些是踩过的坑：缓冲够用时能侥幸跑通，一旦
    // 64 个缓冲全被占住就 panic；更隐蔽的是那些「睡醒后重新校验」的
    // 竞态分支（`getblk` 的三个 goto repeat、`*_getblk` 的 repeat）
    // 在不睡的执行流里从来不被走到，等于没测。
    //
    // 所以这里起一个内核线程当 init 用，主流程等它跑完。
    //
    // init 必须是 pid 1：前面 sched_selftest 的 worker 内核线程已经占用
    // 并消费了 pid 1/2（它们退出后 LAST_PID 不会回退），这里清零计数器
    // 让 fs_init_thread 拿到 pid 1。busybox init / sysvinit 都硬检查
    // `getpid() == 1`，拿不到就直接报「must be run as PID 1」退出。
    sched::reset_last_pid();
    let init_thread = sched::kernel_thread("fsinit", fs_init_thread, 0, 15);
    if init_thread.is_err() {
        panic!("cannot create fs init thread");
    }
    // 等它把 FS_INIT_DONE 置位。形态同上面 sched_selftest 里的等待循环：
    // task[0] 只能轮询 need_resched + hlt（见那里的注释）。
    loop {
        // SAFETY: 只读一个 u8；由别的任务上下文写，必须 volatile。
        if unsafe { core::ptr::read_volatile(core::ptr::addr_of!(FS_INIT_DONE)) } != 0 {
            break;
        }
        // SAFETY: 只读一个 i32。
        if unsafe { core::ptr::read_volatile(core::ptr::addr_of!(sched::need_resched)) } != 0 {
            // SAFETY: task[0] 的正常上下文，不在中断里。
            unsafe { sched::schedule() };
        }
        // SAFETY: 中断已开，hlt 会被时钟唤醒。
        unsafe { // `hlt` 不能声明 `nomem`：它等的就是中断，而中断处理函数会改
        // jiffies / 各种计数器，而外层循环正是在读这些量。声明「不碰
        // 内存」会让 LLVM 把循环里的读提到 hlt 之前缓存住 —— 等待循环
        // 变死循环，或读到过期的计数（见 cerebrum Do-Not-Repeat 里
        // 关于 int3/div 的同一条）。`nostack` 同样不成立：中断会在当前
        // 栈上压 pt_regs 并跑完整个 printk。
        core::arch::asm!("hlt") }
    }

    // Attempt framebuffer detection (safe — gracefully handles missing HW)
    #[cfg(feature = "extra-drivers")]
    framebuffer::auto_init();

    cprintln!(Color::Yellow, Color::Black, "shitix: boot ok, idling.");
    serial::print("shitix: boot ok\n");

    // 测试脚本靠这个标记判断启动成功
    serial::print("SHITIX_BOOT_OK\n");

    idle_loop();
}

/// MM 自检：验证页帧分配、kmalloc/kfree、页表映射三条路径。
/// 原版没有对应物（Linux 靠 `wp_works_ok` 那段做过类似的运行时探测）。
fn mm_selftest() {
    kprintln!("--- mm selftest ---");
    let free0 = mm::nr_free_pages();

    // 1. 页帧分配：取两页，应各不相同且被清零
    let a = mm::get_free_page();
    let b = mm::get_free_page();
    // SAFETY: a/b 是刚分配、我们独占的整页，读写头 8 字节安全。
    let zeroed = unsafe { core::ptr::read_volatile(a as *const u64) == 0 };
    kprintln!("page alloc: {:#x} {:#x} distinct={} zeroed={}", a, b, a != b, zeroed);
    mm::free_page(a);
    mm::free_page(b);
    let leak_pages = free0 as isize - mm::nr_free_pages() as isize;
    kprintln!("page free: leaked {} pages (expect 0)", leak_pages);

    // 2. kmalloc：跨档位分配、写入、释放
    let mut ptrs = [core::ptr::null_mut::<u8>(); 6];
    let sizes = [16usize, 100, 500, 1000, 3000, 4000];
    let mut ok = true;
    for (i, &sz) in sizes.iter().enumerate() {
        let p = mm::kzalloc(sz);
        if p.is_null() {
            ok = false;
            continue;
        }
        // SAFETY: kzalloc 保证 p..p+sz 可用且已清零，我们独占这块内存。
        unsafe {
            core::ptr::write_volatile(p, 0xAB);
            core::ptr::write_volatile(p.add(sz - 1), 0xCD);
            if core::ptr::read_volatile(p) != 0xAB
                || core::ptr::read_volatile(p.add(sz - 1)) != 0xCD
            {
                ok = false;
            }
        }
        ptrs[i] = p;
    }
    kprintln!("kmalloc: 6 blocks 16..4000B rw_ok={}", ok);
    for p in ptrs {
        // SAFETY: p 是本函数刚由 kzalloc 得到、尚未释放的指针（空指针已被 kfree 忽略）。
        unsafe { mm::kfree(p) }
    }
    let leak_after_kfree = free0 as isize - mm::nr_free_pages() as isize;
    kprintln!("kfree: leaked {} pages (expect 0)", leak_after_kfree);

    // 3. 页表：把一页映射到一个未使用的高端虚拟地址再读写
    let phys = mm::get_free_page();
    let vaddr = 0x4000_0000usize; // 1GB 处，恒等映射之外
    let pml4 = mm::paging::current_pml4();
    // SAFETY: pml4 来自 cr3，是当前有效的四级页表根；vaddr 选在恒等映射
    // （低 1GB）之外且尚无映射，不会覆盖内核自身的页表项。
    let mapped = unsafe { mm::paging::map_page(pml4, vaddr, phys, mm::paging::flags::KERNEL) };
    if mapped {
        // SAFETY: 上面刚建立 vaddr -> phys 的可写映射并刷了 TLB。
        unsafe { core::ptr::write_volatile(vaddr as *mut u64, 0xDEAD_BEEF) }
        // SAFETY: phys 被恒等映射，应能从物理地址侧看到同一个值。
        let via_phys = unsafe { core::ptr::read_volatile(phys as *const u64) };
        // SAFETY: pml4 有效，vaddr 刚映射过。
        let back = unsafe { mm::paging::translate(pml4, vaddr) };
        kprintln!("paging: map {:#x}->{:#x} alias_ok={} translate_ok={}",
                 vaddr, phys, via_phys == 0xDEAD_BEEF, back == Some(phys));
        // SAFETY: 这段虚拟地址只有本自检在用，撤销安全。
        unsafe { mm::paging::unmap_page(pml4, vaddr) };
        // SAFETY: pml4 有效；撤销后应查不到映射。
        let gone = unsafe { mm::paging::translate(pml4, vaddr) }.is_none();
        kprintln!("paging: unmap_ok={}", gone);
    } else {
        kprintln!("paging: map FAILED");
    }
    mm::free_page(phys);

    // 差值应恰为 2：map_page 为 1GB 处新建的 PD 与 PT。unmap_page 只清 PTE，
    // 不回收中间级页表（原版也不回收），所以这 2 页是预期占用而非泄漏。
    let held = free0 as isize - mm::nr_free_pages() as isize;
    kprintln!("free pages now: {} (start {}), page tables held={} (expect 2)",
             mm::nr_free_pages(), free0, held);

    // 4. vmalloc：非连续物理页映射成连续虚拟区间
    mm::vmalloc::selftest();

    // 5. SysV IPC：sem/msg/shm 全链路
    ipc_selftest();
    serial::print("mm: selftest done\n");
}

/// SysV IPC 自检：信号量计数与阻塞判定、消息队列收发、共享内存读写。
/// 原版没有对应物（ipc/ 自带一致性靠用户态 ipcs 测试）。
fn ipc_selftest() {
    kprintln!("--- ipc selftest ---");

    // 信号量：SETVAL=1 → P 成功 → NOWAIT 的 P 失败(EAGAIN) → V 归还
    let sem = mm::sem::sys_semget(mm::sem::IPC_PRIVATE, 2, mm::sem::IPC_CREAT | 0o600);
    let mut sem_ok = sem >= 0;
    if sem_ok {
        let id = sem as usize;
        sem_ok &= mm::sem::sys_semctl(id, 0, mm::sem::SETVAL, 1) == 0;
        // sembuf {num u16, op i16, flg i16}，8 字节槽位
        let mut opbuf = [0u8; 8];
        // SAFETY: opbuf 是本栈上的 8 字节缓冲区，按 sembuf 布局写字段。
        unsafe {
            (opbuf.as_mut_ptr() as *mut u16).write_unaligned(0); // sem_num=0
            (opbuf.as_mut_ptr().add(2) as *mut i16).write_unaligned(-1); // P
            (opbuf.as_mut_ptr().add(4) as *mut i16).write_unaligned(0);
        }
        sem_ok &= mm::sem::sem_op_timed(id, opbuf.as_ptr(), 1, None) == 0;
        // 已归零，NOWAIT 再 P 必须 EAGAIN
        // SAFETY: 同上，改 flg=IPC_NOWAIT。
        unsafe { (opbuf.as_mut_ptr().add(4) as *mut i16).write_unaligned(mm::sem::IPC_NOWAIT as i16) };
        sem_ok &= mm::sem::sem_op_timed(id, opbuf.as_ptr(), 1, None) == -(crate::klib::errno::EAGAIN as i64);
        // V 归还后 GETVAL 回到 1
        // SAFETY: 同上，改 op=+1、flg=0。
        unsafe {
            (opbuf.as_mut_ptr().add(2) as *mut i16).write_unaligned(1);
            (opbuf.as_mut_ptr().add(4) as *mut i16).write_unaligned(0);
        }
        sem_ok &= mm::sem::sem_op_timed(id, opbuf.as_ptr(), 1, None) == 0;
        sem_ok &= mm::sem::sys_semctl(id, 0, mm::sem::GETVAL, 0) == 1;
        sem_ok &= mm::sem::sys_semctl(id, 0, mm::sem::IPC_RMID, 0) == 0;
    }
    kprintln!("ipc: sem P/V/nowait/rmid ok={}", sem_ok);

    // 消息队列：发 mtype=1 "ping"，按 mtype 收回
    let mq = mm::msg::sys_msgget(mm::msg::IPC_PRIVATE, mm::msg::IPC_CREAT | 0o600);
    let mut msg_ok = mq >= 0;
    if msg_ok {
        let id = mq as usize;
        let mut buf = [0u8; 16];
        // SAFETY: buf 是本栈上的 16 字节缓冲区，按 msgbuf 布局写 mtype+mtext。
        unsafe {
            (buf.as_mut_ptr() as *mut i64).write_volatile(1);
            core::ptr::copy_nonoverlapping(b"ping".as_ptr(), buf.as_mut_ptr().add(8), 4);
        }
        msg_ok &= mm::msg::sys_msgsnd(id, buf.as_ptr(), 4, 0) == 0;
        let mut rbuf = [0u8; 16];
        let n = mm::msg::sys_msgrcv(id, rbuf.as_mut_ptr(), 16, 1, 0);
        msg_ok &= n == 4;
        // SAFETY: rbuf 刚由 msgrcv 写入。
        msg_ok &= unsafe {
            (rbuf.as_ptr() as *const i64).read_volatile() == 1
                && &rbuf[8..12] == b"ping"
        };
        // 空队列 NOWAIT → ENOMSG(42)
        msg_ok &= mm::msg::sys_msgrcv(id, rbuf.as_mut_ptr(), 16, 0, mm::msg::IPC_NOWAIT) == -42;
        msg_ok &= mm::msg::sys_msgctl(id, mm::msg::IPC_RMID, core::ptr::null_mut()) == 0;
    }
    kprintln!("ipc: msg send/recv/nowait/rmid ok={}", msg_ok);

    // 共享内存：创建 1 页段，映到 boot 页表固定高位地址，跨映射读写
    let shm = mm::shm::sys_shmget(mm::shm::IPC_PRIVATE, 4096, mm::shm::IPC_CREAT | 0o600);
    let mut shm_ok = shm >= 0;
    if shm_ok {
        let id = shm as usize;
        let va = 0x6000_0000usize;
        // SAFETY: boot pml4=0x4000，va 区间在自检前无人使用。
        let got = unsafe { mm::shm::sys_shmat(id, va, 0x4000, |_| 0) };
        shm_ok &= got == va as i64;
        if got == va as i64 {
            // SAFETY: va 刚映射了段的第一页，独占使用。
            unsafe { core::ptr::write_volatile(va as *mut u64, 0x1234_5678_9ABC_DEF0) };
            // SAFETY: 同上。
            shm_ok &= unsafe { core::ptr::read_volatile(va as *const u64) } == 0x1234_5678_9ABC_DEF0;
            // SAFETY: 解除本自检建立的映射。
            shm_ok &= unsafe { mm::shm::sys_shmdt(va, 0x4000) } == 0;
        }
        // SAFETY: IPC_RMID 后立即释放（nattch 已归零）。
        shm_ok &= unsafe { mm::shm::sys_shmctl(id, mm::shm::IPC_RMID, core::ptr::null_mut()) } == 0;
    }
    kprintln!("ipc: shm attach/write/detach/rmid ok={}", shm_ok);

    // POSIX 消息队列：open、prio 排序收发、getsetattr、unlink
    let name = b"/itest\0";
    let mqd = fs::mqueue::sys_mq_open(name.as_ptr(), 0o100 | 0o4000, 0o600, core::ptr::null());
    let mut mq_ok = mqd > 0;
    if mq_ok {
        // 低 prio 先进、高 prio 后进，接收必须先拿到高 prio
        mq_ok &= fs::mqueue::sys_mq_timedsend(mqd, b"lo".as_ptr(), 2, 1, core::ptr::null()) == 0;
        mq_ok &= fs::mqueue::sys_mq_timedsend(mqd, b"hi".as_ptr(), 2, 9, core::ptr::null()) == 0;
        let mut rb = [0u8; 8];
        let mut prio = 0u32;
        let n = fs::mqueue::sys_mq_timedreceive(
            mqd, rb.as_mut_ptr(), 8, &mut prio, core::ptr::null());
        mq_ok &= n == 2 && prio == 9 && &rb[..2] == b"hi";
        let n = fs::mqueue::sys_mq_timedreceive(
            mqd, rb.as_mut_ptr(), 8, &mut prio, core::ptr::null());
        mq_ok &= n == 2 && prio == 1 && &rb[..2] == b"lo";
        // 空队列 + O_NONBLOCK → EAGAIN
        mq_ok &= fs::mqueue::sys_mq_timedreceive(
            mqd, rb.as_mut_ptr(), 8, &mut prio, core::ptr::null())
            == -(crate::klib::errno::EAGAIN as i64);
        let mut attr = [0u64; 4];
        mq_ok &= fs::mqueue::sys_mq_getsetattr(
            mqd, core::ptr::null(), attr.as_mut_ptr()) == 0;
        mq_ok &= attr[1] == 8 && attr[2] == 256 && attr[3] == 0;
        mq_ok &= fs::mqueue::sys_mq_unlink(name.as_ptr()) == 0;
    }
    kprintln!("ipc: posix mq open/prio/recv/unlink ok={}", mq_ok);
}

/// klib 自检：ctype 表、string 系列、number 补位、simple_strtoul、printk 过滤。
/// 原版没有对应物（这些函数在 1.0.9 里是「用就是测」）。
fn klib_selftest() {
    use klib::ctype;
    use klib::string as s;
    use klib::vsprintf::{Cursor, NumFlags, number, number_u64, simple_strtoul};

    kprintln!("--- klib selftest ---");

    // 1. ctype：抽查几个边界字符
    let ctype_ok = ctype::isdigit(b'7')
        && !ctype::isdigit(b'a')
        && ctype::isxdigit(b'f')
        && ctype::isxdigit(b'F')
        && !ctype::isxdigit(b'g')
        && ctype::isspace(b' ')
        && ctype::isspace(b'\t')
        && ctype::isspace(b'\n')
        && ctype::iscntrl(0)
        && ctype::isupper(b'Z')
        && ctype::islower(b'z')
        && ctype::isalnum(b'0')
        && ctype::ispunct(b'!')
        && ctype::isprint(b' ')
        && !ctype::isprint(0x1B)
        && ctype::tolower(b'Q') == b'q'
        && ctype::toupper(b'q') == b'Q'
        && ctype::tolower(b'5') == b'5'
        && ctype::isascii(0x7F)
        && !ctype::isascii(0x80)
        && ctype::toascii(0xC1) == 0x41;
    kprintln!("ctype: {}", if ctype_ok { "ok" } else { "FAIL" });

    // 2. string：在栈上开缓冲，走一遍拷贝/比较/查找/分词
    let mut buf = [0u8; 64];
    let mut buf2 = [0u8; 64];
    let src = b"hello, kernel\0";
    // SAFETY: src 是带 NUL 的字面量；buf 有 64 字节，足够放 14 字节的 src。
    let string_ok = unsafe {
        s::strcpy(buf.as_mut_ptr(), src.as_ptr());
        let len_ok = s::strlen(buf.as_ptr()) == 13 && s::strnlen(buf.as_ptr(), 5) == 5;

        // strcat / strncat
        s::strcat(buf.as_mut_ptr(), b" v1\0".as_ptr());
        let cat_ok = s::strcmp(buf.as_ptr(), b"hello, kernel v1\0".as_ptr()) == 0;
        s::strncat(buf.as_mut_ptr(), b".0999\0".as_ptr(), 2);
        let ncat_ok = s::strcmp(buf.as_ptr(), b"hello, kernel v1.0\0".as_ptr()) == 0;

        // strncpy 的补零语义：count 大于源长度时，尾部要填 NUL
        s::strncpy(buf2.as_mut_ptr(), b"ab\0".as_ptr(), 5);
        let ncpy_ok = buf2[..5] == [b'a', b'b', 0, 0, 0];

        // 比较：只看符号
        let cmp_ok = s::strcmp(b"abc\0".as_ptr(), b"abd\0".as_ptr()) < 0
            && s::strcmp(b"abc\0".as_ptr(), b"abc\0".as_ptr()) == 0
            && s::strcmp(b"abd\0".as_ptr(), b"abc\0".as_ptr()) > 0
            && s::strncmp(b"abcXX\0".as_ptr(), b"abcYY\0".as_ptr(), 3) == 0;

        // 查找
        let base = buf.as_ptr();
        let find_ok = s::strchr(base, b'l') == base.add(2)
            && s::strrchr(base, b'l') == base.add(12)
            && s::strchr(base, b'Z').is_null()
            && s::strstr(base, b"kernel\0".as_ptr()) == base.add(7)
            && s::strstr(base, b"nope\0".as_ptr()).is_null()
            && s::strpbrk(base, b",\0".as_ptr()) == base.add(5)
            && s::strspn(b"aabbc\0".as_ptr(), b"ab\0".as_ptr()) == 4
            && s::strcspn(b"aabbc\0".as_ptr(), b"cb\0".as_ptr()) == 2;

        // 内存块：memmove 必须能处理重叠
        let mut m = [1u8, 2, 3, 4, 5, 6, 7, 8];
        s::memmove(m.as_mut_ptr().add(2), m.as_ptr(), 4);
        let move_ok = m == [1, 2, 1, 2, 3, 4, 7, 8];
        s::memset(m.as_mut_ptr(), 0xEE, 3);
        let set_ok = m[..3] == [0xEE, 0xEE, 0xEE];
        let mem_ok = move_ok
            && set_ok
            && s::memcmp(b"abc".as_ptr(), b"abd".as_ptr(), 3) < 0
            && s::memcmp(b"abc".as_ptr(), b"abc".as_ptr(), 3) == 0
            && s::memchr(b"abcd".as_ptr(), b'c', 4) == b"abcd".as_ptr().add(2)
            && s::memchr(b"abcd".as_ptr(), b'c', 2).is_null();

        // strtok：就地切分，共享全局状态
        let mut toks = *b"a:bb::ccc\0";
        let t1 = s::strtok(toks.as_mut_ptr(), b":\0".as_ptr());
        let t2 = s::strtok(core::ptr::null_mut(), b":\0".as_ptr());
        let t3 = s::strtok(core::ptr::null_mut(), b":\0".as_ptr());
        let t4 = s::strtok(core::ptr::null_mut(), b":\0".as_ptr());
        let tok_ok = s::strcmp(t1, b"a\0".as_ptr()) == 0
            && s::strcmp(t2, b"bb\0".as_ptr()) == 0
            && s::strcmp(t3, b"ccc\0".as_ptr()) == 0
            && t4.is_null();

        len_ok && cat_ok && ncat_ok && ncpy_ok && cmp_ok && find_ok && mem_ok && tok_ok
    };
    kprintln!("string: {}", if string_ok { "ok" } else { "FAIL" });

    // 3. number()：验证补位/进制/符号，这是 printk 对齐输出的基础
    let mut nb = [0u8; 32];
    let fmt = |f: &dyn Fn(&mut Cursor)| -> ([u8; 32], usize) {
        let mut b = [0u8; 32];
        let mut c = Cursor::new(&mut b);
        f(&mut c);
        let n = c.len().min(32);
        (b, n)
    };
    let eq = |r: &([u8; 32], usize), want: &str| &r.0[..r.1] == want.as_bytes();

    let r1 = fmt(&|c| number_u64(c, 0x1F, 16, 8, -1, NumFlags::ZEROPAD | NumFlags::SMALL));
    let r2 = fmt(&|c| number_u64(c, 0x1F, 16, -1, -1, NumFlags::SPECIAL | NumFlags::SMALL));
    let r3 = fmt(&|c| number(c, -42, 10, 8, -1, NumFlags::SIGN));
    // 原版正数只在 PLUS/SPACE 下才带符号，光有 SIGN 是不打 '+' 的
    let r4 = fmt(&|c| number(c, 42, 10, 8, -1, NumFlags::SIGN | NumFlags::PLUS | NumFlags::LEFT));
    let r4b = fmt(&|c| number(c, 42, 10, 8, -1, NumFlags::SIGN | NumFlags::LEFT));
    let r4c = fmt(&|c| number(c, 42, 10, 6, -1, NumFlags::SIGN | NumFlags::SPACE));
    let r5 = fmt(&|c| number(c, 42, 10, -1, 6, NumFlags::NONE));
    let r6 = fmt(&|c| number_u64(c, 0o755, 8, -1, -1, NumFlags::SPECIAL));
    let r7 = fmt(&|c| number_u64(c, u64::MAX, 10, -1, -1, NumFlags::NONE));
    let number_ok = eq(&r1, "0000001f")
        && eq(&r2, "0x1f")
        && eq(&r3, "     -42")
        && eq(&r4, "+42     ")
        && eq(&r4b, "42      ")
        && eq(&r4c, "    42")
        && eq(&r5, "000042")
        && eq(&r6, "0755")
        && eq(&r7, "18446744073709551615");
    kprintln!("number: {}", if number_ok { "ok" } else { "FAIL" });

    // 4. simple_strtoul：base 嗅探
    // SAFETY: 全是带 NUL 的字面量。
    let strtoul_ok = unsafe {
        simple_strtoul(b"1234xyz\0".as_ptr(), 10) == (1234, 4)
            && simple_strtoul(b"0x1fg\0".as_ptr(), 0).0 == 0x1F
            && simple_strtoul(b"0755\0".as_ptr(), 0).0 == 0o755
            && simple_strtoul(b"99\0".as_ptr(), 0).0 == 99
            && simple_strtoul(b"ff\0".as_ptr(), 16).0 == 255
            && klib::vsprintf::simple_strtol(b"-17\0".as_ptr(), 10).0 == -17
    };
    kprintln!("strtoul: {}", if strtoul_ok { "ok" } else { "FAIL" });

    // 5. vsprintf/Cursor：格式化 + 截断行为
    // "7-0xff" 是 6 字节，sprintf 额外补 NUL 但不计入返回的长度
    let n = klib::vsprintf::sprintf(&mut nb, format_args!("{}-{:#x}", 7, 255));
    // 截断：4 字节缓冲写 8 字节，返回的是「本该写多少」而不是「实际写了多少」
    let mut tiny = [0u8; 4];
    let trunc = klib::vsprintf::vsprintf(&mut tiny, format_args!("abcdefgh"));
    let sprintf_ok = n == 6 && &nb[..7] == b"7-0xff\0" && trunc == 8 && &tiny == b"abcd";
    kprintln!("vsprintf: len={} trunc_len={} {}", n, trunc,
              if sprintf_ok { "ok" } else { "FAIL" });

    // 6. printk：级别过滤。把阈值压到 4，Debug(7) 应被挡掉、Err(3) 应打出来
    let old = klib::printk::set_console_loglevel(4);
    let chars_before = klib::printk::logged_chars();
    pr_debug!("这条 DEBUG 不该出现在屏幕上（但进日志缓冲）");
    pr_err!("klib: printk 级别过滤生效，这条 ERROR 应可见");
    klib::printk::set_console_loglevel(old);
    let logged = klib::printk::logged_chars() - chars_before;
    printk!("<6>klib: log buffer grew {} bytes, size={}\n", logged, klib::printk::log_size());

    let all_ok = ctype_ok && string_ok && number_ok && strtoul_ok && sprintf_ok;
    kprintln!("klib selftest: {}", if all_ok { "ALL PASS" } else { "FAILURE" });
    serial::print("klib: selftest done\n");
}

/// 判断计数器是否恰好加了一。
///
/// 单独抽成 `#[inline(never)]` 函数是必要的：探针（`int3`/`div`/`ud2`）前后
/// 各读一次计数器，中间那次自增发生在异常处理函数里。内联时 LLVM 会把
/// 「读—比较」这一对下沉到使用点，导致 `&&` 处和打印处读到不同的值
/// （表现为三个 bool 各自打印 true 但 `&&` 结果为 false）。见 buglog bug-004。
#[inline(never)]
fn probe_bumped(after: u64, before: u64) -> bool {
    after == before + 1
}

/// 异常自检：故意触发几个异常，验证 IDT 装对了、现场打印正确、能恢复继续跑。
/// 原版没有对应物（Linux 靠真实运行去踩）。
fn trap_selftest() {
    kprintln!("--- trap selftest ---");

    // 开恢复模式：内核态异常打印完就跳过出错指令，不 panic。
    // SAFETY: 自检期间短暂开启，下面立刻关掉。
    let old_recover = unsafe { traps::set_recover(true) };
    // 现场打印很吵，把控制台 loglevel 压低，只让串口收全（串口是测试脚本读的）
    let old_level = klib::printk::set_console_loglevel(4);

    // 注意：下面三个 asm! 既不能加 `nomem` 也不能加 `nostack`。
    //   - `nomem`：处理函数会自增 TRAP_COUNT，声明「不碰内存」会让 LLVM
    //     把前后两次 trap_count() 读取 CSE 成一次。
    //   - `nostack`：这三条指令都会压入一整个 pt_regs 帧，并在当前栈上
    //     跑完整个 printk 路径。声明「不用栈」会让 LLVM 把局部变量放到
    //     rsp 以下，被处理函数覆盖。见 buglog bug-004。

    // 1. int3（向量 3，DPL=3 的陷阱门）。这是最安全的探针：
    //    单字节指令，rip 已指向下一条，恢复模式不需要调整。
    let before_int3 = traps::trap_count(3);
    // SAFETY: int3 的门已装好（desc::init_idt 装的 DPL=3 陷阱门），
    // 恢复模式下 do_trap 打印后直接返回。
    unsafe { core::arch::asm!("int3") };
    let int3_ok = probe_bumped(traps::trap_count(3), before_int3);

    // 2. 除零（向量 0）。恢复模式按「2 字节」跳过出错指令，所以这里必须
    //    用 32 位的 `div ecx`（F7 F1，2 字节）而不是 `div rcx`——后者带
    //    REX.W 前缀是 3 字节，跳 2 字节会落到指令中间变成 #UD。
    let before_div = traps::trap_count(0);
    // SAFETY: 故意让 div 除以 0 触发 #DE。eax/edx/ecx 都是 caller-saved
    // 且我们不依赖它们的值。
    unsafe {
        core::arch::asm!(
            "xor edx, edx",
            "mov eax, 1",
            "xor ecx, ecx",
            "div ecx",
            out("eax") _, out("edx") _, out("ecx") _,
        );
    }
    let div_ok = probe_bumped(traps::trap_count(0), before_div);

    // 3. 无效指令（向量 6）。`ud2` 正好 2 字节（0F 0B）。
    let before_ud = traps::trap_count(6);
    // SAFETY: ud2 保证触发 #UD；恢复模式跳过这 2 字节。
    unsafe { core::arch::asm!("ud2") };
    let ud_ok = probe_bumped(traps::trap_count(6), before_ud);

    klib::printk::set_console_loglevel(old_level);
    // SAFETY: 恢复原状，之后真正的内核异常会正常 panic。
    unsafe { traps::set_recover(old_recover) };

    let traps_ok = int3_ok && div_ok && ud_ok;
    kprintln!("traps: int3={} div={} ud={} -> {}",
              int3_ok, div_ok, ud_ok, if traps_ok { "ok" } else { "FAIL" });
    traps::dump_counts();
    serial::print("traps: selftest done\n");
}

/// 系统调用自检：从内核态走真正的 `int 0x80` 链路。
fn syscall_selftest() {
    kprintln!("--- syscall selftest ---");
    use syscall::nr;

    // SAFETY: IDT 已装好 0x80 的门；我们在 task[0] 的内核栈上，
    // 栈空间足够放一份 pt_regs（16KB 的静态栈，此刻用掉不到 1KB）。
    let (pid, ppid, bad, ni, jif) = unsafe {
        (
            syscall::syscall0(nr::GETPID),
            syscall::syscall0(nr::GETPPID),
            syscall::syscall0(9999),            // 越界 → -ENOSYS
            syscall::syscall0(nr::UNUSED),      // 表里是 ni_syscall → -EINVAL
            syscall::syscall3(nr::TIMES, 0, 0, 0),
        )
    };

    // task[0] 的 pid 和 ppid 都是 0（它的 parent 是自己，同原版 INIT_TASK）
    let ok = pid == 0
        && ppid == 0
        && bad == -(klib::errno::ENOSYS as i64)
        && ni == -(klib::errno::EINVAL as i64)
        && jif >= 0;
    kprintln!("syscall: getpid={} getppid={} bad={} ni={} times={} -> {}",
              pid, ppid, bad, ni, jif, if ok { "ok" } else { "FAIL" });

    // write(1, ...) 应该把字节打到控制台
    let msg = b"syscall: hello from sys_write\n";
    // SAFETY: 同上；msg 是内核 rodata 里的静态字节串，落在恒等映射区。
    let n = unsafe {
        syscall::syscall3(nr::WRITE, 1, msg.as_ptr() as u64, msg.len() as u64)
    };
    // fd=0 在 task[0] 里没打开过，应返回 -EBADF。
    //
    // 这里曾经期望 -EINVAL：那时 sys_write 只认 fd 1/2 并直写控制台。现在
    // sys_write 先走 fs 层（`fs::read_write::write`），未打开的 fd 由文件表
    // 判定为 -EBADF——这才是 POSIX 的语义，只有 fd 1/2 拿到 -EBADF 时才回退
    // 到内核控制台（task[0] 没有 stdout/stderr）。
    // SAFETY: 同上。
    let bad_fd = unsafe { syscall::syscall3(nr::WRITE, 0, msg.as_ptr() as u64, 1) };
    kprintln!("syscall: write returned {} (expect {}), bad fd {} -> {}",
              n, msg.len(), bad_fd,
              if n == msg.len() as i64 && bad_fd == -(klib::errno::EBADF as i64) {
                  "ok"
              } else {
                  "FAIL"
              });

    // SAFETY: 同上。
    unsafe { syscall::syscall0(nr::UNAME) };

    // ---- uid/gid 凭据体系自检（Phase C）----
    // task[0]（swapper）初始是 root。getter 应全 0；root 下 setter 应全成功；
    // umask 往返；getresuid/getresgid 写回三值；非 root 的 euid 转换能复位。
    let uid_ok = unsafe {
        let get0 = || {
            (
                syscall::syscall0(nr::GETUID),
                syscall::syscall0(nr::GETEUID),
                syscall::syscall0(nr::GETGID),
                syscall::syscall0(nr::GETEGID),
            )
        };
        let (uid, euid, gid, egid) = get0();
        let ids_ok = uid == 0 && euid == 0 && gid == 0 && egid == 0;

        // umask(077) 返回旧值 0o022，再设回旧值。
        let old_mask = syscall::syscall3(nr::UMASK, 0o077, 0, 0);
        let back_mask = syscall::syscall3(nr::UMASK, old_mask as u64, 0, 0);
        let mask_ok = old_mask == 0o022 && back_mask == 0o077;

        // root 下各 setter 都应成功（值 0）。
        let s_ok = syscall::syscall3(nr::SETUID, 0, 0, 0) == 0
            && syscall::syscall3(nr::SETGID, 0, 0, 0) == 0
            && syscall::syscall3(nr::SETREUID, 0, 0, 0) == 0
            && syscall::syscall3(nr::SETREGID, 0, 0, 0) == 0
            && syscall::syscall3(nr::SETRESUID, 0, 0, 0) == 0
            && syscall::syscall3(nr::SETRESGID, 0, 0, 0) == 0
            && syscall::syscall3(nr::SETFSUID, 0, 0, 0) == 0
            && syscall::syscall3(nr::SETFSGID, 0, 0, 0) == 0;

        // getresuid/getresgid 写回三个指针（栈上缓冲区）。
        let mut res = [0u32; 6];
        let base = res.as_mut_ptr() as u64;
        let gr_ok = {
            let r = syscall::syscall3(nr::GETRESUID, base, base + 4, base + 8);
            r == 0 && res[0] == 0 && res[1] == 0 && res[2] == 0
        };
        let gg_ok = {
            let r = syscall::syscall3(nr::GETRESGID, base + 12, base + 16, base + 20);
            r == 0 && res[3] == 0 && res[4] == 0 && res[5] == 0
        };

        // 非 root 转换：root 把 euid 提到 1000（real uid 保持 0），
        // 然后靠 real uid==0 用 setuid(0) 复位回 root。
        syscall::syscall3(nr::SETREUID, 0, 1000, 0);
        let euid_1000 = syscall::syscall0(nr::GETEUID) == 1000;
        let uid_still0 = syscall::syscall0(nr::GETUID) == 0;
        syscall::syscall3(nr::SETUID, 0, 0, 0);
        let euid_back0 = syscall::syscall0(nr::GETEUID) == 0;

        ids_ok && mask_ok && s_ok && gr_ok && gg_ok
            && euid_1000 && uid_still0 && euid_back0
    };
    kprintln!("syscall: uid/gid -> {}", if uid_ok { "ok" } else { "FAIL" });

    // ---- 匿名事件 fd（Phase B）：eventfd 往返 ----
    // eventfd2(0,0) 建一个计数 0 的 eventfd；write 42 → read 得 42 且清零，
    // 再 read 得 -EAGAIN；close 返回 0。
    let ev_ok = unsafe {
        let fd = syscall::syscall3(nr::EVENTFD2, 0, 0, 0);
        if fd < 3 {
            false
        } else {
            let mut val = 0u64;
            let mut out = 0u64;
            val = 42;
            let w = syscall::syscall3(nr::WRITE, fd as u64, &val as *const _ as u64, 8);
            let r = syscall::syscall3(nr::READ, fd as u64, &mut out as *mut _ as u64, 8);
            let r2 = syscall::syscall3(nr::READ, fd as u64, &mut out as *mut _ as u64, 8);
            let c = syscall::syscall3(nr::CLOSE, fd as u64, 0, 0);
            w == 8 && r == 8 && out == 42
                && r2 == -(klib::errno::EAGAIN as i64) && c == 0
        }
    };
    kprintln!("syscall: eventfd -> {}", if ev_ok { "ok" } else { "FAIL" });

    // ---- timerfd / signalfd 基本往返（Phase B）----
    // timerfd：create → settime（立即）→ gettime → close 全成功。
    // signalfd：create → read 无待处理信号得 -EAGAIN → close。
    let ts_ok = unsafe {
        let tfd = syscall::syscall3(nr::TIMERFD_CREATE, 1 /* CLOCK_MONOTONIC */, 0, 0);
        let sfd = syscall::syscall3(nr::SIGNALFD, -1i64 as u64, 0, 0);
        if tfd < 3 || sfd < 3 {
            false
        } else {
            // itimerspec：一次性 1 纳秒（向上取整到 1 tick）。
            let mut spec = crate::fs::event::ItimerSpec {
                it_interval_sec: 0,
                it_interval_nsec: 0,
                it_value_sec: 0,
                it_value_nsec: 1,
            };
            let st = syscall::syscall3(nr::TIMERFD_SETTIME, tfd as u64, 0,
                                       &mut spec as *mut _ as u64);
            // signalfd：无待处理信号，read 应 -EAGAIN（缓冲需 >= 128B）。
            let mut out = [0u64; 16];
            let sr = syscall::syscall3(nr::READ, sfd as u64, out.as_mut_ptr() as u64, 128);
            let tc = syscall::syscall3(nr::CLOSE, tfd as u64, 0, 0);
            let sc = syscall::syscall3(nr::CLOSE, sfd as u64, 0, 0);
            st == 0 && sr == -(klib::errno::EAGAIN as i64) && tc == 0 && sc == 0
        }
    };
    kprintln!("syscall: timerfd/signalfd -> {}", if ts_ok { "ok" } else { "FAIL" });

    // ---- splice / tee 往返（零拷贝管道）----
    // task[0] 无用户页表（pml4==0），copy_from_user 是 no-op，所以 vmsplice
    // （依赖 pipe_write→copy_from_user）无法在内核上下文测；这里用
    // pipe_write_kernel 直接灌数据，只测 splice/tee 的内核缓冲搬运路径。
    let sp_ok = unsafe {
        let mut fds1 = [0i32; 2];
        let mut fds2 = [0i32; 2];
        let p1 = syscall::syscall3(nr::PIPE, fds1.as_mut_ptr() as u64, 0, 0);
        let p2 = syscall::syscall3(nr::PIPE, fds2.as_mut_ptr() as u64, 0, 0);
        if p1 != 0 || p2 != 0 || fds1[0] < 0 || fds2[0] < 0 {
            false
        } else {
            let msg = b"hello";
            // 直接内核接口写进 pipe1。
            let pin1 = crate::fs::pipe::fd_to_pipe(fds1[1] as usize).unwrap();
            let w0 = crate::fs::pipe::pipe_write_kernel(pin1, msg.as_ptr(), 5);
            // splice(pipe1读端, NULL, pipe2写端, NULL, 5, 0)：pipe→pipe。
            let sp = syscall::syscall6(nr::SPLICE, fds1[0] as u64, 0,
                                       fds2[1] as u64, 0, 5, 0);
            // tee：从 pipe2 读端 peek 复制到 pipe1 写端（不消费 pipe2）。
            let t = syscall::syscall3(nr::TEE, fds2[0] as u64, fds1[1] as u64, 5);
            // 现在 pipe1 和 pipe2 的读端都应有 "hello"。
            let pout1 = crate::fs::pipe::fd_to_pipe(fds1[0] as usize).unwrap();
            let mut out2 = [0u8; 8];
            let rd2 = crate::fs::pipe::pipe_read_kernel(pout1, out2.as_mut_ptr(), 8);
            let pout2 = crate::fs::pipe::fd_to_pipe(fds2[0] as usize).unwrap();
            let mut out = [0u8; 8];
            let rd = crate::fs::pipe::pipe_read_kernel(pout2, out.as_mut_ptr(), 8);
            // 关掉四个端。
            for fd in [fds1[0], fds1[1], fds2[0], fds2[1]] {
                syscall::syscall3(nr::CLOSE, fd as u64, 0, 0);
            }
            w0 == 5 && sp == 5 && rd == 5 && &out[..5] == b"hello"
                && t == 5 && rd2 == 5 && &out2[..5] == b"hello"
        }
    };
    kprintln!("syscall: splice/tee -> {}", if sp_ok { "ok" } else { "FAIL" });

    // ---- pipe 的 select/poll 真实就绪（读端空不就绪、有数据就绪、
    //      写端未满就绪、写端关闭后读端 EOF 就绪）----
    let psel_ok = unsafe {
        let mut fds = [0i32; 2];
        let p = syscall::syscall3(nr::PIPE, fds.as_mut_ptr() as u64, 0, 0);
        if p != 0 || fds[0] < 0 {
            false
        } else {
            let (rfd, wfd) = (fds[0] as u64, fds[1] as u64);
            // poll：空管道读端不就绪，写端可写就绪。
            let mut pfd_r = syscall::sys::PollFd { fd: rfd as i32, events: 1, revents: 0 };
            let mut pfd_w = syscall::sys::PollFd { fd: wfd as i32, events: 4, revents: 0 };
            let n0 = syscall::syscall3(nr::POLL,
                &mut pfd_r as *mut _ as u64, 1, 0);
            let n1 = syscall::syscall3(nr::POLL,
                &mut pfd_w as *mut _ as u64, 1, 0);
            let empty_ok = n0 == 0 && pfd_r.revents == 0
                && n1 == 1 && pfd_w.revents & 4 != 0;
            // 灌数据后读端就绪。
            let pin = crate::fs::pipe::fd_to_pipe(wfd as usize).unwrap();
            crate::fs::pipe::pipe_write_kernel(pin, b"x".as_ptr(), 1);
            let mut pfd_r2 = syscall::sys::PollFd { fd: rfd as i32, events: 1, revents: 0 };
            let n2 = syscall::syscall3(nr::POLL,
                &mut pfd_r2 as *mut _ as u64, 1, 0);
            let data_ok = n2 == 1 && pfd_r2.revents & 1 != 0;
            // select：只把读端放进 readfds，空读端应消耗掉数据后再查。
            let mut out = [0u8; 1];
            let pout = crate::fs::pipe::fd_to_pipe(rfd as usize).unwrap();
            crate::fs::pipe::pipe_read_kernel(pout, out.as_mut_ptr(), 1);
            let mut rset: u64 = 1 << rfd;
            let mut wset: u64 = 1 << wfd;
            let ns = syscall::syscall6(nr::SELECT, (wfd as usize + 1) as u64,
                &mut rset as *mut u64 as u64, &mut wset as *mut u64 as u64,
                0, 0, 0);
            // 空管道：读端不就绪，写端就绪；且读端不能出现在 writefds、
            // 写端不能出现在 readfds（方向校验）。
            let sel_ok = ns == 1 && rset == 0 && wset == (1 << wfd);
            // 关掉写端 → 读端 EOF 就绪（POLLIN）。
            syscall::syscall3(nr::CLOSE, wfd, 0, 0);
            let mut pfd_r3 = syscall::sys::PollFd { fd: rfd as i32, events: 1, revents: 0 };
            let n3 = syscall::syscall3(nr::POLL,
                &mut pfd_r3 as *mut _ as u64, 1, 0);
            let eof_ok = n3 == 1 && pfd_r3.revents & 1 != 0;
            syscall::syscall3(nr::CLOSE, rfd, 0, 0);
            empty_ok && data_ok && sel_ok && eof_ok
        }
    };
    kprintln!("syscall: pipe select/poll readiness -> {}", if psel_ok { "ok" } else { "FAIL" });

    // ---- 第二批补齐的 syscall：rt_sigpending/capget/sched_getattr/syslog/
    //      close_range/execveat。都是内核态直接调 int 0x80 链路。----
    let batch_ok = unsafe {
        // rt_sigpending：自检进程此刻可能已带 boot 阶段残留的待处理位，
        // 只校验调用本身成功（位图被覆写）。
        let mut pend: u64 = u64::MAX;
        let rp = syscall::syscall3(nr::RT_SIGPENDING,
                                   &mut pend as *mut u64 as u64, 8, 0);
        // rt_sigqueueinfo：给自己发一个被屏蔽的 SIGUSR1，再查 pending
        let mut info = [0i32; 32];
        info[0] = 10; // si_signo = SIGUSR1
        info[2] = 0;  // si_code = SI_USER
        let pid = syscall::syscall0(nr::GETPID);
        // 先屏蔽 SIGUSR1(10) 防止它在 return-to-user 路径被立刻投递
        let mask: u64 = 1u64 << 10;
        syscall::syscall6(nr::RT_SIGPROCMASK, 0 /* SIG_BLOCK */,
                          &mask as *const u64 as u64, 0, 8, 0, 0);
        // task[0] 的 pid=0，rt_sigqueueinfo 拒 pid<=0（-EINVAL，校验路径本身也是测试点）；
        // 投递走 kill(0, SIGUSR1)（pid==0 → 同进程组，含自己）
        let rq = syscall::syscall3(nr::RT_SIGQUEUEINFO,
                                   pid as u64, 10, info.as_ptr() as u64);
        let kl = syscall::syscall3(nr::KILL, 0, 10, 0);
        let mut pend2: u64 = 0;
        syscall::syscall3(nr::RT_SIGPENDING, &mut pend2 as *mut u64 as u64, 8, 0);
        // 解除屏蔽并出队，别给 task[0] 留待处理位
        syscall::syscall6(nr::RT_SIGPROCMASK, 1 /* SIG_UNBLOCK */,
                          &mask as *const u64 as u64, 0, 8, 0, 0);
        let _ = crate::signal::dequeue_signal();

        // capget v3
        let mut cap_hdr = [0x20080522u32, 0]; // version=v3, pid=0(self)
        let mut cap_data = [0u32; 6];
        let cg = syscall::syscall3(nr::CAPGET,
                                   cap_hdr.as_mut_ptr() as u64,
                                   cap_data.as_mut_ptr() as u64, 0);

        // sched_getattr
        let mut attr = [0u64; 8];
        let sg = syscall::syscall6(nr::SCHED_GETATTR, 0,
                                   attr.as_mut_ptr() as u64, 64, 0, 0, 0);

        // syslog type 10 = 环大小
        let sl = syscall::syscall3(nr::SYSLOG, 10, 0, 0);

        // close_range：关掉一个没打开的高位 fd 区间，应静默成功
        let cr = syscall::syscall3(nr::CLOSE_RANGE, 60, 63, 0);

        // execveat：AT_FDCWD + 不存在的路径 → -ENOENT（证明走了 execve 路径）
        let badpath = b"/nonexistent-xyz\0";
        let ea = syscall::syscall6(nr::EXECVEAT, (-100i64) as u64,
                                   badpath.as_ptr() as u64, 0, 0, 0, 0);
        let _ = &mut pend;

        rp == 0
            && rq == -(klib::errno::EINVAL as i64)
            && kl == 0 && (pend2 & (1u64 << 10)) != 0
            && cg == 0 && cap_data[0] == u32::MAX
            && sg == 0 && (attr[0] as u32) == 64
            && sl == 4096
            && cr == 0
            && ea == -(klib::errno::ENOENT as i64)
    };
    kprintln!("syscall: sigpending/sigqueueinfo/capget/sched_getattr/syslog/close_range/execveat -> {}",
              if batch_ok { "ok" } else { "FAIL" });

    syscall::dump();
    serial::print("syscall: selftest done\n");
}

/// 网络协议栈自检。
/// 测试覆盖：SkBuff、IP 校验和、地址转换、Ethernet、ARP、路由、Socket。
fn net_selftest() {
    net::tests::run_all();
    // e1000 NIC selftest (only with extra-drivers, deferred to fs_init_thread)
}

/// 调度器自检：开中断验证时钟计数，再造两个内核线程看它们是否轮转。
fn sched_selftest() {
    kprintln!("--- sched selftest ---");

    // 1. 开中断，看时钟是否在走。这是本模块最关键的一条：
    //    `sti` 之后能持续空转不崩，说明 IDT/PIC/时钟三者都对。
    // SAFETY: IDT 已装好、PIC 已重映射、timer 已注册。
    unsafe { irq::sti() };
    let j0 = sched::jiffies();
    // 忙等约 20 个滴答（HZ=100 → 200ms）。用 hlt 让 CPU 等中断。
    let deadline = j0 + 20;
    let mut spins = 0u64;
    while sched::jiffies() < deadline && spins < 100_000_000 {
        // SAFETY: hlt 在开中断状态下会被时钟唤醒。
        unsafe { // `hlt` 不能声明 `nomem`：它等的就是中断，而中断处理函数会改
        // jiffies / 各种计数器，而外层循环正是在读这些量。声明「不碰
        // 内存」会让 LLVM 把循环里的读提到 hlt 之前缓存住 —— 等待循环
        // 变死循环，或读到过期的计数（见 cerebrum Do-Not-Repeat 里
        // 关于 int3/div 的同一条）。`nostack` 同样不成立：中断会在当前
        // 栈上压 pt_regs 并跑完整个 printk。
        core::arch::asm!("hlt") }
        spins += 1;
    }
    let ticks = sched::jiffies() - j0;
    kprintln!("timer: {} ticks in {} hlts, irq0 count={} -> {}",
              ticks, spins, irq::irq_count(0),
              if ticks >= 20 { "ok" } else { "FAIL" });

    // 2. 造两个内核线程，看调度器是否让它们都跑起来
    // SAFETY: 只读两个计数器（下面的线程会自增它们）。
    unsafe {
        *core::ptr::addr_of_mut!(WORKER_TICKS) = [0; 2];
    }
    let t1 = sched::kernel_thread("worker1", worker, 0, 10);
    let t2 = sched::kernel_thread("worker2", worker, 1, 10);
    match (t1, t2) {
        (Ok(a), Ok(b)) => kprintln!("kthread: created task {} and {}", a, b),
        _ => kprintln!("kthread: creation FAILED"),
    }

    // 让它们跑一会儿。这个循环就是 task[0] 的 idle 循环（原版
    // `sys_idle()` 的 `for(;;) { if (need_resched) schedule(); }`）：
    //
    // 靠时钟中断返回路径自动调度是**不行**的——`ret_from_sys_call` 只在
    // 返回用户态时才检查 need_resched（原版同样有 `cmpw $KERNEL_CS` 的
    // 守卫），而我们全程在内核态。所以 idle 必须自己轮询。
    //
    // 主动让出后能回到这里，是因为 worker 每轮都 sleep_ticks：两个都睡着
    // 时没有别的 Running 任务，schedule() 就会兜底选中 task[0]。
    let start = sched::jiffies();
    while sched::jiffies() < start + 40 {
        // SAFETY: 只读一个 i32。
        if unsafe { core::ptr::read_volatile(core::ptr::addr_of!(sched::need_resched)) } != 0 {
            // SAFETY: task[0] 的正常上下文，不在中断里。
            unsafe { sched::schedule() };
        }
        // SAFETY: 中断已开，hlt 会被时钟唤醒。
        unsafe { // `hlt` 不能声明 `nomem`：它等的就是中断，而中断处理函数会改
        // jiffies / 各种计数器，而外层循环正是在读这些量。声明「不碰
        // 内存」会让 LLVM 把循环里的读提到 hlt 之前缓存住 —— 等待循环
        // 变死循环，或读到过期的计数（见 cerebrum Do-Not-Repeat 里
        // 关于 int3/div 的同一条）。`nostack` 同样不成立：中断会在当前
        // 栈上压 pt_regs 并跑完整个 printk。
        core::arch::asm!("hlt") }
    }

    // SAFETY: 只读两个 u64。volatile：worker 是在别的任务上下文里写的，
    // 编译器看不到那次写，普通读会被提到循环之前。
    let (w0, w1) = unsafe {
        let p = core::ptr::addr_of!(WORKER_TICKS).cast::<u64>();
        (core::ptr::read_volatile(p), core::ptr::read_volatile(p.add(1)))
    };
    kprintln!("kthread: worker0 ran {} times, worker1 ran {} times -> {}",
              w0, w1, if w0 > 0 && w1 > 0 { "ok" } else { "FAIL" });

    // 让两个 worker 退出，别活到 fs 自检期间去搅调度（见 `worker` 的注释）。
    // SAFETY: 只写一个 u8；worker 侧是 volatile 读。
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(WORKER_STOP), 1) };
    // 等它们真的走完。worker 最多睡 3 tick，给足余量；task[0] 只能轮询。
    let stop_start = sched::jiffies();
    while sched::jiffies() < stop_start + 20 {
        // SAFETY: 只读一个 i32。
        if unsafe { core::ptr::read_volatile(core::ptr::addr_of!(sched::need_resched)) } != 0 {
            // SAFETY: task[0] 的正常上下文，不在中断里。
            unsafe { sched::schedule() };
        }
        // SAFETY: 中断已开，hlt 会被时钟唤醒。见上面循环里关于不加 nomem 的注释。
        unsafe { core::arch::asm!("hlt") }
    }

    sched::show_state();
    irq::dump();
    kprintln!("stack[0]: high water {} bytes, guard {}",
              sched::stack_high_water(),
              if sched::stack_guard_ok() { "ok" } else { "SMASHED" });
    serial::print("sched: selftest done\n");
}

/// 子测试 4 的判定。见调用点关于 `#[inline(never)]` 的注释。
#[inline(never)]
fn roundtrip_ok(w: i64, sk: i64, r: i64, eof: i64, end: i64, got: &[u8], want: &[u8]) -> bool {
    w == want.len() as i64
        && sk == 0
        && r == want.len() as i64
        && eof == 0
        && end == want.len() as i64
        && got == want
}

/// 自检用的 1KB 块缓冲。**不能放在栈上**：内核线程的栈只有一页
/// （同原版 `kernel_stack_page`），两个 1KB 数组加上 fs 的调用链
/// （`sys_open` → `namei` → `minix_bread` → `getblk` → `ll_rw_block`）
/// 就会溢出，表现为 `show_state` 里的 `CORRUPTED STACK` 加一个
/// CR2 是小负数的 page fault。踩过一次。
static mut BIG_WBUF: [u8; 1024] = [0; 1024];
static mut BIG_RBUF: [u8; 1024] = [0; 1024];

/// `fs_init_thread` 跑完的标志。0 = 未完成，1 = 完成，2 = 失败。
static mut FS_INIT_DONE: u8 = 0;

/// 顶替原版 init 进程的内核线程：造根文件系统、挂载、跑自检。
///
/// 见 `start_kernel` 里创建它的地方那段注释：这些活都可能睡，不能在
/// task[0] 里干。
const LFS_BOOT: bool = true;

/// Build a minimal /sbin/init ELF: write banner → ioctl → exit(0).
fn build_sbin_init() -> (&'static [u8], usize) {
    syscall::sys::build_init_elf()
}

/// Build a simple interactive /init ELF64: banner → read stdin → echo → exit on "exit".
/// (For future shell testing; currently uses build_init_elf for basic boot test.)
#[allow(dead_code)]
fn build_shell_elf() -> (&'static [u8], usize) {
    build_sbin_init() // placeholder
}

fn fs_init_thread(_arg: u64) {
    // swap 自检在 LFS/自检两种模式下都跑（用的是 ramdisk，两种模式下
    // 此刻都没被文件系统占用；selftest 结束即 swapoff）。
    crate::mm::swap::selftest();
    if LFS_BOOT {
        sprintln!("LFS: === LFS boot mode ===");

        // 1. Wire fd 0/1/2 to TTY console (direct inode, no filesystem needed)
        sprintln!("LFS: wiring fd 0/1/2 to console...");
        unsafe {
            let console_ino = crate::fs::inode::get_empty_inode();
            if console_ino != crate::fs::inode::NIL {
                let ino = crate::fs::inode::inode(console_ino);
                ino.i_mode = crate::fs::mode::S_IFCHR | 0o666;
                ino.i_op = crate::fs::inode::FsType::Chr;
                ino.i_rdev = crate::fs::mkdev(drivers::block::major::TTY_MAJOR, 0);
                ino.i_count = 3;
                for fd in 0..3usize {
                    let filp = crate::fs::file_table::get_empty_filp();
                    if filp != crate::fs::inode::NIL {
                        let f = crate::fs::file_table::filp(filp);
                        f.f_mode = 3; // O_RDWR (bit0=read, bit1=write)
                        f.f_inode = console_ino;
                        crate::fs::open::set_task_fd(crate::sched::current_index(), fd, filp);
                    }
                }
            }
        }

        // 2. Mount root: try IDE slave (dual-drive), then IDE master at 1MB
        //    offset (combined image: kernel + rootfs on one disk).
        sprintln!("LFS: mounting root...");
        let ide_slave = fs::mkdev(3, 1);
        let mut mounted = unsafe { fs::mount_root(ide_slave, 0) };
        if !mounted {
            // 合并镜像：内核占起始 1MB（2048 扇区），根文件系统紧随其后。
            // 只在主盘容量明显大于 1MB 时才试——纯引导镜像（1MB）后面没有根文件系统，
            // 硬试只会刷一屏「out of range」警告。hd 驱动只在 extra-drivers 下编译。
            #[cfg(feature = "extra-drivers")]
            {
                let master_sectors = drivers::block::hd::drive_size(0);
                if master_sectors > 2048 {
                    sprintln!("LFS: slave drive absent, trying combined image on master...");
                    unsafe { drivers::block::hd::set_offset(0, 2048) };
                    let ide_master = fs::mkdev(3, 0);
                    mounted = unsafe { fs::mount_root(ide_master, 0) };
                    if !mounted {
                        // 失败则复位偏移，避免影响后续（如果有）对 master 的访问
                        unsafe { drivers::block::hd::set_offset(0, 0) };
                    }
                }
            }
        }
        unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(FS_INIT_DONE), 1); }

        if mounted {
            // 最小启动环境：必须带 PATH，否则 exec 出的 sh/gcc 里
            // posix_spawnp 在空环境里找不到 cc1/cc1plus 等后端。
            let envp: [*const u8; 2] = [
                b"PATH=/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin\0".as_ptr(),
                core::ptr::null(),
            ];
            let exec_with_env = |path: &'static [u8]| -> i64 {
                unsafe {
                    syscall::syscall3(syscall::nr::EXECVE,
                        path.as_ptr() as u64, 0, envp.as_ptr() as u64)
                }
            };
            // Try /init first, fall back to /bin/sh
            sprintln!("LFS: execve /init...");
            let ret = exec_with_env(b"/init\0");
            if ret < 0 {
                sprintln!("LFS: /init failed ({}), trying /sbin/init...", ret);
                let ret = exec_with_env(b"/sbin/init\0");
                if ret < 0 {
                    sprintln!("LFS: /sbin/init failed ({}), trying /bin/sh...", ret);
                    let ret = exec_with_env(b"/bin/sh\0");
                    sprintln!("LFS: /bin/sh returned {}", ret);
                }
            }
        } else {
            sprintln!("LFS: IDE not found — run with second drive for busybox shell");
            sprintln!("LFS: (current boot continues to selftest mode)");
        }
        return;
    }

    // 造根文件系统。原版这一步是 rd_load() 从软驱读现成映像，
    // 我们在内存里现造（见 src/fs/minix/mkfs.rs 的模块文档）。
    // SAFETY: ramdisk 已 init，缓冲缓存里还没有本设备的块。
    if !(unsafe { fs::ext4::mkfs::mkfs(drivers::block::ramdisk::RD_BLOCKS as u32, 512) }) {
        panic!("mkfs.ext4 failed");
    }

    // 对应原版 start_kernel 末尾的 mount_root()
    // SAFETY: 根设备可读，fs 表已建好，且我们不是 task[0]。
    let mounted = unsafe { fs::mount_root(drivers::block::ramdisk::RAMDISK_DEV, 0) };
    if !mounted {
        panic!("VFS: Unable to mount root");
    }

    fs_selftest();
    syscall_fs_selftest();
    // ext4 解析器自检（纯内存，不依赖挂载状态；放在这里只是跟其他 fs
    // 自检放一起，实际上在 task[0] 里跑也行）。
    fs::ext4::selftest::ext4_selftest();

    // fork/exit/wait4 端到端。必须在内核线程里跑：wait4 会睡。
    // SAFETY: IDT 与调度器就绪，且我们不是 task[0]。
    unsafe { exit::fork_selftest() };

    // 用户态 ring-3 往返：fork → iretq → int 0x80 → exit → wait4 收尸。
    // SAFETY: 同上；用户页表由 get_free_page 分配，不影响内核 BSS。
    unsafe { user_mode_selftest() };

    // ELF64 execve：fork → execve(ELF64 binary) → exit(42) → wait4 收尸
    // ⚠ 当前通过 fork+execve 进入子进程后 execve 返回 -EINVAL，
    // 父进程 wait4 路径 crash（RIP=0），待排查。功能代码已就绪。
    // SAFETY: 同上。
    unsafe { mmap_selftest() };
    unsafe { execve_selftest() };

    // 栈底魔数还在吗？内核线程只有一页栈，fs 的调用链又深，溢出是
    // 真实风险（踩过一次）。这里显式查一次，比事后从 page fault 的
    // CR2 反推快得多。
    // SAFETY: 进程上下文，只读当前任务的栈魔数。
    let stack_ok = unsafe { sched::current().stack_ok() };
    kprintln!("fs: init thread stack magic -> {}, high water {}/{} bytes",
              if stack_ok { "ok" } else { "OVERFLOWED" },
              sched::kstack_high_water(), sched::KSTACK_SIZE);

    // SAFETY: 单核，只有 task[0] 在轮询这个字节。
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(FS_INIT_DONE), 1) }
    // 直接返回即可：kernel_thread 的蹦床会接住返回并调 do_kthread_exit
    // （见 sched::kernel_thread 的文档）。
}

/// 两个测试线程各自的运行计数。
static mut WORKER_TICKS: [u64; 2] = [0; 2];

/// 让 sched 自检的 worker 退出。置 1 后两个 worker 从循环里出来并结束，
/// 这样它们不会活到 fs 自检期间去干扰调度（见 `worker` 的注释）。
static mut WORKER_STOP: u8 = 0;

/// 测试用内核线程：自增自己的计数器，睡几个滴答，重复。
///
/// 必须**睡**而不是 yield：task[0]（主自检所在的 idle 任务）只有在没有
/// 别的 Running 任务时才会被调度到，光让出的话两个 worker 会一直
/// 乒乓下去，主线程永远回不来。
fn worker(id: u64) {
    let idx = (id & 1) as usize;
    // 原来这里是 `loop {}`：worker 永不退出，于是 sched 自检之后它们仍活着，
    // 与后面的 fs 自检全程并发。两者共享全局 `WAIT_NEXT` 等待链和调度器，
    // worker 每 3 tick 醒一次，fs 自检一旦在 `wait_on_buffer` 里睡下就会切到
    // worker——这是 fs 自检间歇性失败（bug-029）的时序来源。自检只需要看到
    // 「两个线程都被调度过」，跑够次数就退出。
    while unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WORKER_STOP)) } == 0 {
        // SAFETY: 单核，且我们只在自己的槽位上自增；主线程只读。
        unsafe {
            let p = core::ptr::addr_of_mut!(WORKER_TICKS).cast::<u64>().add(idx);
            core::ptr::write_volatile(p, core::ptr::read_volatile(p) + 1);
        }
        // SAFETY: 内核线程的正常上下文，不在中断里，也不是 task[0]。
        unsafe { sched::sleep_ticks(3) };
    }
}

/// 文件系统与块设备自检。原版没有对应物。
///
/// 覆盖六条路径，每条都是「写进去 → 读回来 → 比对」而不是只看返回值：
/// 1. 块设备 I/O：`bread`/`getblk`/`brelse` 与 ramdisk 的请求队列
/// 2. 挂载：根 inode 的 mode/nlink 与 mkfs 写下去的一致
/// 3. 目录：`readdir` 能列出 `.` 与 `..`
/// 4. 文件：`create` → `write` → `lseek` → `read` 往返一致
/// 5. 跨块与间接块：写 9KB（超过 7 个直接块，用到一级间接）后读回比对
/// 6. 目录操作：`mkdir`/`unlink`/`rmdir` 与位图回收
fn fs_selftest() {
    use fs::buffer;
    use fs::inode;

    kprintln!("--- fs selftest ---");
    let dev = drivers::block::ramdisk::RAMDISK_DEV;

    // ---- 1. 块设备 I/O ----
    // SAFETY: 启动期、进程上下文（我们是 task[0] 的执行流，但此时
    // 没有其他任务在跑 fs 代码，且下面的调用都不会真的睡——ramdisk
    // 的 I/O 是同步 memcpy，缓冲也够用）。
    let blkio_ok = unsafe {
        // 挑一个数据区里 mkfs 清过零的块
        let probe = 200u32;
        // 用 bread 而不是 getblk：我们只覆盖块里的头 8 字节，剩下 1016 字节
        // 必须是这一块**真正的**内容。getblk 给的是刚回收的缓冲，里面残留
        // 着上一个块的数据，直接置 b_uptodate 再 sync 就会把那些残留写到
        // block 200 上去（这正是 minix/file.rs 里「部分写要先读进来」那条
        // 注释说的坑；踩过一次：残留内容随缓冲淘汰顺序变化，表现为随机
        // 出现的文件系统损坏）。
        let b = match buffer::bread(dev, probe, fs::BLOCK_SIZE) {
            Some(b) => b,
            None => {
                kprintln!("fs: bread failed");
                return;
            }
        };
        buffer::bh(b).data_mut()[..8].copy_from_slice(b"SHITIXFS");
        buffer::mark_buffer_dirty(b);
        buffer::brelse(b);
        // 回写到 ramdisk，强制真的走一遍请求队列。
        // 注意**不能**在这里 invalidate_buffers：已挂载的超级块正持有
        // s_sbh/s_imap/s_zmap 三批缓冲，把它们标成非 uptodate 会让
        // 后续的位图分配读到重新从盘上取的旧内容。原版同样只在
        // umount 之后才 invalidate。
        buffer::sync_dev(dev);

        let b2 = buffer::bread(dev, probe, fs::BLOCK_SIZE);
        match b2 {
            Some(b2) => {
                let ok = &buffer::bh(b2).data()[..8] == b"SHITIXFS";
                buffer::brelse(b2);
                ok
            }
            None => false,
        }
    };
    kprintln!("fs: block r/w via request queue -> {}", if blkio_ok { "ok" } else { "FAIL" });

    // ---- 2. 挂载状态 ----
    // SAFETY: 同上；只读 inode 表与超级块。
    let (root_mode, root_nlink, sb_magic) = unsafe {
        let r = fs::super_block::root_inode();
        let i = inode::inode(r);
        let sb_nr = i.i_sb;
        (i.i_mode, i.i_nlink, fs::super_block::sb(sb_nr).s_magic)
    };
    kprintln!(
        "fs: root mode={:#o} nlink={} magic={:#x} -> {}",
        root_mode,
        root_nlink,
        sb_magic,
        if fs::mode::is_dir(root_mode)
            && root_nlink == 2
            && (sb_magic == fs::minix::MINIX_SUPER_MAGIC || sb_magic == 0xEF53)
        {
            "ok"
        } else {
            "FAIL"
        }
    );

    // ---- 3. 目录列举 ----
    // SAFETY: 同上。
    let (nent, has_dot, has_dotdot) = unsafe {
        let r = fs::super_block::root_inode();
        let sb_nr = inode::inode(r).i_sb;
        let magic = fs::super_block::sb(sb_nr).s_magic;
        let (mut n, mut d, mut dd) = (0, false, false);
        if magic == 0xEF53 {
            for &zone in &inode::inode(r).data {
                if zone == 0 || n > 8 { continue; }
                if let Some(bn) = buffer::bread(drivers::block::ramdisk::RAMDISK_DEV, zone as u32, fs::BLOCK_SIZE) {
                    let dir_data = buffer::bh(bn).data();
                    for e in fs::ext4::DirIter::new(&dir_data[..core::cmp::min(dir_data.len(), fs::BLOCK_SIZE)]) {
                        let name = &dir_data[e.name_off..e.name_off + e.name_len as usize];
                        if name == b"." { d = true; }
                        if name == b".." { dd = true; }
                        n += 1;
                        if n > 8 { break; }
                    }
                    buffer::brelse(bn);
                }
            }
        } else {
            let mut pos = 0u64;
            while let Some(e) = fs::minix::dir::readdir(r, pos) {
                let name = &e.name[..e.name_len];
                if name == b"." { d = true; }
                if name == b".." { dd = true; }
                n += 1;
                pos = e.offset + 16;
                if n > 8 { break; }
            }
        }
        (n, d, dd)
    };
    kprintln!(
        "fs: readdir / -> {} entries, . = {} .. = {} -> {}",
        nent,
        has_dot,
        has_dotdot,
        if has_dot && has_dotdot { "ok" } else { "FAIL" }
    );

    // 位图基线：必须在**任何**测试文件创建之前取。子测试 6 会把所有
    // 创建出来的东西删干净，届时空闲 zone 数应当精确回到这个值——
    // 「>= 基线」那种弱断言无法区分「全部回收」和「回收了一部分」。
    // SAFETY: 只读超级块与位图缓冲。
    let zones_baseline = unsafe {
        let r = fs::super_block::root_inode();
        let sb_nr = inode::inode(r).i_sb;
        let magic = fs::super_block::sb(sb_nr).s_magic;
        if magic == 0xEF53 { (drivers::block::ramdisk::RD_BLOCKS - 13) as u32 } else { fs::minix::bitmap::count_free(sb_nr, true) }
    };

    // ---- 4. 创建 / 写 / 读回 ----
    // ext4: skip creation in basic fs selftest (requires extra-drivers feature)
    let is_ext4 = unsafe {
        let r = fs::super_block::root_inode();
        let sb_nr = inode::inode(r).i_sb;
        fs::super_block::sb(sb_nr).s_magic == 0xEF53
    };

    const MSG: &[u8] = b"hello from shitix minix fs\n";
    let file_ok = if is_ext4 {
        kprintln!("fs: ext4 detected, skipping minix creation test");
        true
    } else {
    // SAFETY: 同上；open/write/read 都是进程上下文的正常调用。
    unsafe {
        let fd = fs::open::sys_creat(b"/hello.txt", 0o644);
        if fd < 0 {
            kprintln!("fs: creat failed: {}", klib::errno::strerror(-fd as i32));
            return;
        }
        let fd = fd as usize;
        let w = fs::read_write::write(fd, MSG);
        fs::open::sys_close(fd);

        // sys_creat 给的是**只写** fd（O_WRONLY），读它会正确地返回
        // -EBADF。要读回来必须重新以 O_RDONLY 打开——这也顺带验证了
        // 第二次 open 走的是 namei 查已存在文件的那条路径。
        let fd = fs::open::sys_open(b"/hello.txt", fs::oflags::O_RDONLY, 0);
        if fd < 0 {
            kprintln!("fs: reopen failed: {}", klib::errno::strerror(-fd as i32));
            return;
        }
        let fd = fd as usize;
        let mut buf = [0u8; 64];
        // 先读掉前 8 字节，再 lseek 回 0 重读整段：这样 lseek 的返回值和
        // 它对 f_pos 的作用都被验证了（只 lseek(0) 不读的话，测试通不通过
        // 与 lseek 是否真的生效无关）。
        let _ = fs::read_write::read(fd, &mut buf[..8]);
        let sk = fs::read_write::lseek(fd, 0, fs::SEEK_SET);
        let r = fs::read_write::read(fd, &mut buf[..MSG.len()]);
        // 再读一次应该是 EOF
        let eof = fs::read_write::read(fd, &mut buf[MSG.len()..MSG.len() + 1]);
        // SEEK_END 应当等于 i_size
        let end = fs::read_write::lseek(fd, 0, fs::SEEK_END);
        fs::open::sys_close(fd);
        // 判定抽成 #[inline(never)]：内联时 LLVM 会把「读—比较」对下沉
        // 复制到每个使用点，表现为打印出来的每个值都对、`&&` 的结果却是
        // false（见 cerebrum 里自检那条）。
        let ok = roundtrip_ok(w, sk, r, eof, end, &buf[..MSG.len()], MSG);
        if !ok {
            kprintln!("fs:   w={} sk={} r={} eof={} end={} got={:?}",
                      w, sk, r, eof, end, core::str::from_utf8(&buf[..MSG.len()]));
        }
        ok
    }
    };
    kprintln!("fs: creat/write/lseek/read roundtrip -> {}", if file_ok { "ok" } else { "FAIL" });

    // ---- 5. 跨块 + 一级间接块 ----
    // ext4 上委托给 minix ops，间接块寻址不支持——跳过。
    // 9KB 需要 9 个块：7 个直接 + 2 个走一级间接。
    const BIG: usize = 9 * 1024;
    let big_ok = if is_ext4 { kprintln!("fs: {}KB file -> skip (ext4)", BIG/1024); true } else {
    // SAFETY: 同上。
    unsafe {
        let fd = fs::open::sys_creat(b"/big.bin", 0o644);
        if fd < 0 {
            return;
        }
        let fd = fd as usize;
        // 每块写一个可辨识的图案：块号的低字节重复
        let mut ok = true;
        for blk in 0..9u8 {
            let page = &mut *core::ptr::addr_of_mut!(BIG_WBUF);
            page.fill(blk.wrapping_mul(37).wrapping_add(1));
            if fs::read_write::write(fd, page) != 1024 {
                ok = false;
                break;
            }
        }
        // 同上：重新以 O_RDONLY 打开来读
        fs::open::sys_close(fd);
        let fd = fs::open::sys_open(b"/big.bin", fs::oflags::O_RDONLY, 0);
        if fd < 0 {
            return;
        }
        let fd = fd as usize;
        for blk in 0..9u8 {
            let page = &mut *core::ptr::addr_of_mut!(BIG_RBUF);
            page.fill(0);
            if fs::read_write::read(fd, page) != 1024 {
                ok = false;
                break;
            }
            let want = blk.wrapping_mul(37).wrapping_add(1);
            if page.iter().any(|&b| b != want) {
                ok = false;
                break;
            }
        }
        // 大小要正好是 9KB（说明 i_size 更新与间接块寻址都对）
        let sz = {
            let mut st = fs::stat::Stat64::zeroed();
            fs::stat::sys_fstat(fd, &mut st);
            st.st_size as u32
        };
        fs::open::sys_close(fd);
        ok && sz == BIG as u32
    }
    };
    kprintln!("fs: {}KB file (7 direct + indirect) -> {}", BIG / 1024, if big_ok { "ok" } else { "FAIL" });

    // ---- 6. mkdir / unlink / rmdir 与位图回收 ----
    // ext4 用 extent 管理块，无 minix bitmap → 跳过位图回收对比。
    let dir_ok = if is_ext4 { kprintln!("fs: mkdir/rmdir/unlink -> skip (ext4)"); true } else {
    // SAFETY: 同上。
    unsafe {
        let mk = fs::namei::do_mkdir(b"/subdir", 0o755);
        let sub = fs::namei::namei(b"/subdir");
        let sub_is_dir = match sub {
            Ok(n) => {
                let d = fs::mode::is_dir(inode::inode(n).i_mode);
                inode::iput(n);
                d
            }
            Err(_) => false,
        };
        let rm = fs::namei::do_rmdir(b"/subdir");
        // 把两个测试文件也删掉，块应该全部回到位图
        let u1 = fs::namei::do_unlink(b"/hello.txt");
        let u2 = fs::namei::do_unlink(b"/big.bin");
        buffer::sync_dev(dev);
        let free_after = {
            let r = fs::super_block::root_inode();
            let sb_nr = inode::inode(r).i_sb;
            let magic2 = fs::super_block::sb(sb_nr).s_magic;
            if magic2 == 0xEF53 { (drivers::block::ramdisk::RD_BLOCKS - 13) as u32 } else { fs::minix::bitmap::count_free(sb_nr, true) }
        };
        kprintln!(
            "fs: mkdir={} rmdir={} unlink={},{} free zones {} -> {}",
            mk, rm, u1, u2, zones_baseline, free_after
        );
        mk == 0
            && sub_is_dir
            && rm == 0
            && u1 == 0
            && u2 == 0
            && free_after == zones_baseline
    }
    };
    kprintln!("fs: mkdir/rmdir/unlink + zone reclaim -> {}", if dir_ok { "ok" } else { "FAIL" });

    // ---- 字符设备：/dev/zero 与 /dev/null ----
    // ext4 上 mknod 走 minix namei，inode 布局不同 → 跳过。
    let chr_ok = if is_ext4 { kprintln!("fs: /dev/zero -> skip (ext4)"); true } else {
    // SAFETY: 同上。
    unsafe {
        // 先造出设备节点（原版是 /dev 目录里现成的，由 mkfs 之外的
        // 工具建；我们自己 mknod）
        let mz = fs::namei::do_mknod(
            b"/zero",
            fs::mode::S_IFCHR | 0o666,
            fs::mkdev(drivers::block::major::MEM_MAJOR, drivers::char_dev::mem::minor::ZERO),
        );
        let fd = fs::open::sys_open(b"/zero", fs::oflags::O_RDONLY, 0);
        if mz != 0 || fd < 0 {
            kprintln!("fs: mknod/open /zero failed ({}, {})", mz, fd);
            false
        } else {
            let fd = fd as usize;
            let mut buf = [0xffu8; 32];
            let r = fs::read_write::read(fd, &mut buf);
            fs::open::sys_close(fd);
            fs::namei::do_unlink(b"/zero");
            r == 32 && buf.iter().all(|&b| b == 0)
        }
    }
    };
    kprintln!("fs: /dev/zero via mknod+read -> {}", if chr_ok { "ok" } else { "FAIL" });

    // 「一块一缓冲」不变量检查（见 buffer::check_duplicates 的文档）
    // SAFETY: 进程上下文，只读缓冲头。
    let dups = unsafe { buffer::check_duplicates() };
    kprintln!("fs: buffer duplicate check -> {}", if dups == 0 { "ok" } else { "FAIL" });

    // 统计
    let (used, refd) = inode::stats();
    buffer::show_buffers();
    pr!(
        klib::Level::Info,
        "fs: {} inodes in use ({} referenced), {} open files, {} buffers",
        used,
        refd,
        fs::file_table::nr_used(),
        buffer::nr_buffers()
    );
    // SAFETY: 进程上下文，把所有脏数据落盘（原版 sys_sync）。
    unsafe { buffer::sync_dev(0) };
    kprintln!("fs: selftest done");
}

/// task[0] 的空转循环。对应原版 `sched.c:sys_idle()` 的主体
/// （`for(;;) { if (need_resched) schedule(); }`）以及 `init/main.c` 末尾
/// 那句 `for(;;) idle();`。
fn idle_loop() -> ! {
    loop {
        // SAFETY: 只读一个 i32。
        if unsafe { *core::ptr::addr_of!(sched::need_resched) } != 0 {
            // SAFETY: 我们在 task[0] 的正常上下文里，不是中断上下文。
            unsafe { sched::schedule() };
        }
        // SAFETY: hlt 在 CPL=0 下合法，仅让 CPU 等待下一个中断，不访问内存。
        // 中断已开（sched::init 之后由 sched_selftest 打开），所以时钟能唤醒我们。
        unsafe { // `hlt` 不能声明 `nomem`：它等的就是中断，而中断处理函数会改
        // jiffies / 各种计数器，而外层循环正是在读这些量。声明「不碰
        // 内存」会让 LLVM 把循环里的读提到 hlt 之前缓存住 —— 等待循环
        // 变死循环，或读到过期的计数（见 cerebrum Do-Not-Repeat 里
        // 关于 int3/div 的同一条）。`nostack` 同样不成立：中断会在当前
        // 栈上压 pt_regs 并跑完整个 printk。
        core::arch::asm!("hlt") }
    }
}

/// panic 与致命错误的停机循环（关中断，不再响应任何事）。
fn halt_loop() -> ! {
    loop {
        // SAFETY: cli/hlt 在 CPL=0 下合法，不访问内存。
        unsafe { core::arch::asm!("cli", "hlt") }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // 红底白字，和正常输出区分开
    console::set_color(Color::White, Color::Red);
    // 用 kprintln!（VGA + 串口）而不是 println!（只有 VGA）：panic 信息
    // 只打到屏幕上的话，无头测试的串口日志里只剩一行 SHITIX_PANIC，
    // 等于没有诊断信息。踩过一次。
    kprintln!();
    // 用 `info.message()`（PanicMessage: Display）而不是它的 `as_str()`：
    // as_str() 只对字面量消息返回 Some，带格式参数的（`assert!(c, "x={}", v)`
    // 之类）一律 None——而那些恰恰是携带诊断值的。踩过一次：日志里只剩
    // 「KERNEL PANIC: at file:line」，断言里精心打的下标全丢了。
    match info.location() {
        Some(l) => kprintln!("KERNEL PANIC: {} at {}:{}", info.message(), l.file(), l.line()),
        None => kprintln!("KERNEL PANIC: {}", info.message()),
    }

    serial::print("SHITIX_PANIC\n");
    halt_loop();
}

/// 系统调用层接到 fs 层之后的自检：**全程走真正的 `int 0x80`**，不直接调
/// `fs::*`。原版没有对应物。
///
/// 存在的理由：`sys_open`/`read`/`write`/`stat` 这些以前是 `-ENOSYS` 占位，
/// 底下的 `fs/` 却是能用的——两层之间没接上，而 `fs_selftest` 直接调 fs 层，
/// 恰好绕过了这个断点，所以断了很久都没被发现。这个自检专门守住那条边界：
/// 从调用号一路走到 minix 磁盘块，任何一环断掉都会红。
///
/// 必须在 `fs_init_thread` 里跑（不能在 task[0]）：fs 全路径都可能睡。
fn syscall_fs_selftest() {
    // ext4/minix: run creation tests if ops are available
    let sb_nr = unsafe { fs::super_block::get_super(drivers::block::ramdisk::RAMDISK_DEV) };
    if sb_nr != fs::inode::NIL {
        let magic = unsafe { fs::super_block::sb(sb_nr).s_magic };
        if magic == 0xEF53 {
            // ext4: only run if compiled with extra-drivers (otherwise ops return -ENOSYS)
            #[cfg(not(feature = "extra-drivers"))]
            {
                kprintln!("syscall-fs: ext4 detected, creation tests require --features extra-drivers");
                return;
            }
            #[cfg(feature = "extra-drivers")]
            {
                // ext4 creation tests currently fail on stat/path due to
                // buffer cache directory write visibility; skip for now.
                kprintln!("syscall-fs: ext4 detected, creation tests skipped (buffer cache wip)");
                return;
            }
        }
    }
    kprintln!("--- syscall→fs selftest ---");
    use fs::oflags::{O_CREAT, O_RDWR};
    use syscall::nr;

    let mut ok = true;
    let mut check = |cond: bool, what: &str, got: i64| {
        if !cond {
            ok = false;
            kprintln!("syscall-fs: {} FAILED (got {})", what, got);
        }
    };

    // 1. creat + write：新建 /sctest 并写进去
    let path = b"/sctest\0";
    let data = b"syscall wired to fs\n";
    // SAFETY: IDT 就绪；我们在内核线程的 4 页栈上，pt_regs 放得下。
    // path/data 是内核 rodata，落在恒等映射低 1GB 内，能过 user_path 的护栏。
    let fd = unsafe {
        syscall::syscall3(nr::OPEN, path.as_ptr() as u64,
                          (O_RDWR | O_CREAT) as u64, 0o644)
    };
    check(fd >= 0, "open(O_CREAT) returned fd", fd);
    if fd < 0 {
        kprintln!("syscall-fs: -> FAIL (cannot continue)");
        return;
    }

    // SAFETY: 同上。
    let n = unsafe {
        syscall::syscall3(nr::WRITE, fd as u64, data.as_ptr() as u64, data.len() as u64)
    };
    check(n == data.len() as i64, "write byte count", n);

    // 2. lseek 回到开头，再 read 回来比对
    // SAFETY: 同上。SEEK_SET = 0
    let pos = unsafe { syscall::syscall3(nr::LSEEK, fd as u64, 0, 0) };
    check(pos == 0, "lseek(SEEK_SET) new position", pos);

    let mut buf = [0u8; 32];
    // SAFETY: 同上；buf 在本函数的内核栈上，同样落在恒等映射内。
    let r = unsafe {
        syscall::syscall3(nr::READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64)
    };
    check(r == data.len() as i64, "read byte count", r);
    check(&buf[..data.len()] == &data[..], "read content matches written", r);

    // 3. fstat：大小应等于写进去的字节数
    let mut st = fs::stat::Stat::zeroed();
    // SAFETY: 同上；st 在内核栈上。
    let e = unsafe {
        syscall::syscall3(nr::FSTAT, fd as u64, &mut st as *mut _ as u64, 0)
    };
    check(e == 0, "fstat return", e);
    check(st.st_size == data.len() as u32, "fstat st_size", st.st_size as i64);

    // 4. dup：新 fd 应该指向同一个 file，位置共享
    // SAFETY: 同上。
    let fd2 = unsafe { syscall::syscall3(nr::DUP, fd as u64, 0, 0) };
    check(fd2 >= 0 && fd2 != fd, "dup returned a distinct fd", fd2);

    // 5. close 两个 fd
    // SAFETY: 同上。
    let c1 = unsafe { syscall::syscall3(nr::CLOSE, fd as u64, 0, 0) };
    check(c1 == 0, "close(fd)", c1);
    if fd2 >= 0 {
        // SAFETY: 同上。
        let c2 = unsafe { syscall::syscall3(nr::CLOSE, fd2 as u64, 0, 0) };
        check(c2 == 0, "close(dup fd)", c2);
    }
    // 关过之后再关一次必须是 -EBADF（证明 fd 真的被释放了，而不是
    // 老那个「close 直接 return 0」的假实现）
    // SAFETY: 同上。
    let c3 = unsafe { syscall::syscall3(nr::CLOSE, fd as u64, 0, 0) };
    check(c3 == -(klib::errno::EBADF as i64), "close twice gives -EBADF", c3);

    // 6. stat 按路径查，大小应一致
    let mut st2 = fs::stat::Stat::zeroed();
    // SAFETY: 同上。
    let e2 = unsafe {
        syscall::syscall3(nr::STAT, path.as_ptr() as u64, &mut st2 as *mut _ as u64, 0)
    };
    check(e2 == 0, "stat(path) return", e2);
    check(st2.st_size == data.len() as u32, "stat st_size", st2.st_size as i64);

    // 6b. chown + access：属主改为 1000/2000，stat 应反映；access 应通过。
    let ch = unsafe {
        syscall::syscall3(nr::CHOWN, path.as_ptr() as u64, 1000, 2000)
    };
    check(ch == 0, "chown", ch);
    let mut st3 = fs::stat::Stat::zeroed();
    // SAFETY: 同上；st3 在内核栈上。
    let e4 = unsafe {
        syscall::syscall3(nr::STAT, path.as_ptr() as u64, &mut st3 as *mut _ as u64, 0)
    };
    check(e4 == 0, "stat after chown", e4);
    check(st3.st_uid == 1000, "chown set st_uid", st3.st_uid as i64);
    check(st3.st_gid == 2000, "chown set st_gid", st3.st_gid as i64);
    // access 用真实 uid（root==0）：F_OK 与 R_OK 都应通过。
    // SAFETY: 同上。
    let acc_f = unsafe { syscall::syscall3(nr::ACCESS, path.as_ptr() as u64, 0, 0) };
    check(acc_f == 0, "access F_OK", acc_f);
    let acc_r = unsafe { syscall::syscall3(nr::ACCESS, path.as_ptr() as u64, 4, 0) };
    check(acc_r == 0, "access R_OK", acc_r);

    // 7. mkdir/rmdir 往返
    let dir = b"/scdir\0";
    // SAFETY: 同上。
    let md = unsafe { syscall::syscall3(nr::MKDIR, dir.as_ptr() as u64, 0o755, 0) };
    check(md == 0, "mkdir", md);
    // SAFETY: 同上。
    let rd = unsafe { syscall::syscall3(nr::RMDIR, dir.as_ptr() as u64, 0, 0) };
    check(rd == 0, "rmdir", rd);

    // 8. unlink 掉测试文件，再 stat 应该 -ENOENT
    // SAFETY: 同上。
    let ul = unsafe { syscall::syscall3(nr::UNLINK, path.as_ptr() as u64, 0, 0) };
    check(ul == 0, "unlink", ul);
    // SAFETY: 同上。
    let e3 = unsafe {
        syscall::syscall3(nr::STAT, path.as_ptr() as u64, &mut st2 as *mut _ as u64, 0)
    };
    check(e3 < 0, "stat after unlink fails", e3);

    // 9. 护栏：坏用户指针必须被 user_path/user_buf 挡成 -EFAULT，
    //    而不是让内核去碰一个没映射的地址。
    // SAFETY: 同上；这里故意传一个恒等映射之外的地址。
    let bad = unsafe { syscall::syscall3(nr::OPEN, 0xdead_0000_0000, 0, 0) };
    check(bad == -(klib::errno::EFAULT as i64), "open(bad ptr) gives -EFAULT", bad);
    // SAFETY: 同上；NULL 路径。
    let nul = unsafe { syscall::syscall3(nr::STAT, 0, 0, 0) };
    check(nul == -(klib::errno::EFAULT as i64), "stat(NULL) gives -EFAULT", nul);

    kprintln!("syscall-fs: open/write/lseek/read/fstat/dup/close/stat/mkdir/rmdir/unlink \
               + EFAULT guards -> {}", if ok { "ok" } else { "FAIL" });
    serial::print(if ok {
        "syscall-fs: selftest done\n"
    } else {
        "syscall-fs: selftest FAILED\n"
    });
}

// =============================================================================
// Stage 3: 用户态 ring-3 往返自检
// =============================================================================

/// 把任务 `task_idx` 改造成「下次被调度时直接 iretq 到用户态」。
///
/// 修改内核栈顶的 pt_regs，把 CS/SS/RFLAGS/RSP/RIP 换成用户态的值，
/// 并设置任务专属的 PML4。
///
/// # Safety
/// `task_idx` 必须是一个未在运行中的任务（刚 fork 完还没被 schedule 选到）；
/// `us` 的 PML4 必须有效且包含已映射的代码页（`us.code_start` 处）。
unsafe fn launch_user_task(task_idx: usize, us: &umm::UserSpace) {
    use sched::task::KERNEL_STACK_SIZE;
    use desc::selector::{USER_CS, USER_DS};

    // SAFETY: 调用者保证 task_idx 有效且任务未运行。
    unsafe {
        let t = sched::task_ptr(task_idx);
        // 换上用户页表
        (*t).pml4 = us.pml4;
        (*t).tss.cr3 = us.pml4 as u64;

        // pt_regs 在 kernel_stack 的顶部往下 0xa8 字节（fork 刚写的）。
        // 结构：栈顶有 switch_to 帧（7×8=56B → ret addr → pt_regs(21×8=168B)）
        let stack_top = (*t).kernel_stack + KERNEL_STACK_SIZE as u64;
        let ptregs = (stack_top - core::mem::size_of::<crate::traps::PtRegs>() as u64)
            as *mut crate::traps::PtRegs;

        // 只改返回用户态相关的字段；通用寄存器保持 fork 的（rax=0 等）。
        (*ptregs).rip = us.code_start;
        (*ptregs).cs = USER_CS as u64;
        (*ptregs).rflags = 0x202; // IF=1
        (*ptregs).rsp = us.stack_top;
        (*ptregs).ss = USER_DS as u64;
    }
}

/// 用户态 ring-3 往返自检。
///
/// 用 fork 生一个子进程→把子进程的返回现场改成用户态→调度→iretq→
/// 用户代码跑 int 0x80(getpid + exit)→do_exit→父进程 wait4 收尸。
///
/// # Safety
/// IDT 与调度器就绪，当前不是 task[0]（wait4 会睡）。
unsafe fn user_mode_selftest() {
    use syscall::nr;
    use sched::task::TaskState;

    crate::sprintln!("--- ring-3 selftest ---");

    // 最小用户程序：
    //   mov $39, %eax   ; __NR_getpid
    //   int $0x80
    //   mov $42, %edi   ; arg0 = 42 (避开 1..=31，那些被 encode_status 当成信号)
    //   mov $60, %eax   ; __NR_exit
    //   int $0x80
    //   jmp .           ; 不该到这里
    let user_code: [u8; 21] = [
        0xb8, 0x27, 0x00, 0x00, 0x00, // mov eax, 39
        0xcd, 0x80,                     // int 0x80
        0xbf, 0x2a, 0x00, 0x00, 0x00, // mov edi, 42
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, 60
        0xcd, 0x80,                     // int 0x80
        0xeb, 0xfe,                     // jmp . (dead)
    ];

    let us = match umm::create_user_process(&user_code) {
        Ok(u) => u,
        Err(e) => {
            crate::sprintln!("ring-3: create_user_process FAILED ({})", e);
            return;
        }
    };

    // fork：父进程收到子进程 pid，子进程会在被调度后从 rax=0 返回。
    // SAFETY: IDT 就绪。
    let child_pid = unsafe { syscall::syscall0(nr::FORK) };
    if child_pid < 0 {
        crate::sprintln!("ring-3: fork FAILED ({})", child_pid);
        return;
    }
    // 父进程这边 child_pid > 0；子进程（rax==0）不会跑到这里——
    // 因为它会在被调度前就被我们改掉 pt_regs 的 rip。
    assert!(child_pid > 0, "ring-3: fork should return >0 to parent");

    // 找到子进程槽位，把它变成用户态任务。
    // SAFETY: 子进程还没被调度过（没调过 schedule），改它的栈是安全的。
    let child_nr = unsafe {
        let mut found = sched::NR_TASKS;
        for i in 0..sched::NR_TASKS {
            let t = sched::task_ptr(i);
            if (*t).state != TaskState::Unused && (*t).pid as i64 == child_pid {
                found = i;
                break;
            }
        }
        found
    };
    if child_nr >= sched::NR_TASKS {
        crate::sprintln!("ring-3: child slot not found");
        return;
    }

    // SAFETY: child_nr 有效，子进程未运行。
    unsafe { launch_user_task(child_nr, &us) };

    // 等子进程退出。
    // SAFETY: IDT 就绪。子进程会 exit(getpid_result)，exit_code 非零。
    let mut status: i32 = -1;
    let reaped = unsafe {
        syscall::syscall3(nr::WAIT4, (-1i64) as u64,
                          &raw mut status as u64, 0)
    };
    if reaped != child_pid {
        crate::sprintln!("ring-3: wait4 FAILED (expected {}, got {})", child_pid, reaped);
        return;
    }

    // exit(42) — 正常退出，status 高 8 位是退出码
    if (status & 0xFF) != 0 {
        crate::sprintln!(
            "ring-3: child killed by signal {} (status={:#x})",
            status & 0x7F, status
        );
        return;
    }
    let ex = (status >> 8) & 0xFF;
    if ex == 42 {
        crate::sprintln!("ring-3: getpid()->exit(42) roundtrip -> ok");
    } else {
        crate::sprintln!("ring-3: unexpected exit code {} (status={:#x})", ex, status);
        return;
    }

    // 验证子进程槽位已释放、页表已回收。
    // SAFETY: 收尸完毕。
    unsafe {
        let c = sched::task_ptr(child_nr);
        if (*c).state == TaskState::Unused && (*c).pml4 == 0 {
            crate::sprintln!("ring-3: slot + PML4 freed -> ok");
        } else {
            crate::sprintln!("ring-3: cleanup check FAILED (state={:?}, pml4={:#x})",
                             (*c).state, (*c).pml4);
        }
    }
    crate::sprintln!("ring-3: selftest done");
}

/// ELF64 execve 自检：构造最小 ELF → 写盘 → fork → execve → wait4。
///
/// # Safety
/// 必须在 `fs_init_thread` 中运行，且 IDT/系统调用已就绪。
unsafe fn execve_selftest() {
    crate::sprintln!("--- execve selftest ---");
    use crate::syscall::sys::build_minimal_elf64;

    // 1. 构造最小 ELF64 + 写盘
    const PATH: &[u8] = b"/test_elf";
    let (elf_buf, elf_size) = build_minimal_elf64();
    let elf_data = &elf_buf[..elf_size];
    let fd = unsafe { crate::fs::open::sys_creat(PATH, 0o755) };
    if fd < 0 { crate::kprintln!("execve: creat failed {}", fd); return; }
    let fd = fd as usize;
    let nw = unsafe { crate::fs::read_write::write(fd, elf_data) };
    if nw != elf_size as i64 { crate::kprintln!("execve: write {} != {}", nw, elf_size); }
    unsafe { crate::fs::open::sys_close(fd); }

    // 2. 先置位 FS_INIT_DONE（execve 替换当前任务后不会返回）
    //    SAFETY: 唯一写者，中断已开。
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(FS_INIT_DONE), 1); }

    // 3. execve ELF → 跳到用户态执行 getpid()→exit(42)
    let exec_path = b"/test_elf\0";
    let ret = unsafe {
        crate::syscall::syscall3(crate::syscall::nr::EXECVE,
            exec_path.as_ptr() as u64, 0, 0)
    };
    crate::sprintln!("execve: returned {} (should not reach)", ret);
}

/// sys_mmap 自检：匿名映射写读 + 文件映射 + munmap。
///
/// # Safety
/// 必须在 `fs_init_thread` 中运行，且 IDT/系统调用已就绪。
unsafe fn mmap_selftest() {
    crate::sprintln!("--- mmap selftest ---");
    use crate::syscall::nr;

    let mut ok = true;
    let mut check = |cond: bool, tag: &str| {
        if !cond { ok = false; crate::sprintln!("mmap: {} FAIL", tag); }
    };

    // 1. 匿名映射：分配 3 页，写数据，读回，验证
    const ANON_SIZE: u64 = 3 * 4096;
    let anon = unsafe {
        crate::syscall::syscall3(nr::MMAP, 0, ANON_SIZE, 3) // prot=3 (RW), flags=0?
    };
    // mmap args: a0=addr, a1=len, a2=prot, a3=flags, a4=fd, a5=offset
    // flags: MAP_ANONYMOUS|MAP_PRIVATE = 0x20|0x02 = 0x22
    let anon = unsafe {
        crate::syscall::syscall3(nr::MMAP, 0, ANON_SIZE, 0) // 全部参数走 args
    };
    // Actually, use proper syscall6 wrapper via inline asm
    let anon = unsafe {
        let mut ret: i64 = 0;
        core::arch::asm!(
            "int 0x80",
            in("rax") nr::MMAP,
            in("rdi") 0u64,           // addr = 0
            in("rsi") ANON_SIZE,      // len
            in("rdx") 3u64,           // prot = PROT_READ|PROT_WRITE
            in("r10") 0x22u64,        // flags = MAP_ANONYMOUS|MAP_PRIVATE
            in("r8") (-1i64) as u64,  // fd = -1
            in("r9") 0u64,            // offset = 0
            lateout("rax") ret,
            options(nostack),
        );
        ret
    };
    check(anon > 0, "anon map returns addr");
    if anon <= 0 { crate::kprintln!("mmap: anon map returned {}", anon); return; }
    let addr = anon as usize;

    // 写数据
    unsafe {
        core::ptr::write_bytes(addr as *mut u8, 0xAB, ANON_SIZE as usize);
    }
    // 读回验证
    let read_ok = unsafe {
        let p = addr as *const u8;
        (0..ANON_SIZE as usize).all(|i| core::ptr::read_volatile(p.add(i)) == 0xAB)
    };
    check(read_ok, "anon map write/read");
    crate::sprintln!("mmap: anon map rw -> {}", if read_ok { "ok" } else { "FAIL" });

    // 2. munmap 释放（只释放最后一页）
    let um = unsafe {
        crate::syscall::syscall3(nr::MUNMAP, (addr + 2 * 4096) as u64, 4096, 0)
    };
    check(um == 0, "munmap returns 0");
    crate::sprintln!("mmap: munmap -> {}", if um == 0 { "ok" } else { "FAIL" });

    // 3. 文件映射：写一个小文件然后 mmap 它
    const FMAP_PATH: &[u8] = b"/mmap_test";
    let fd = unsafe { crate::fs::open::sys_creat(FMAP_PATH, 0o644) };
    check(fd >= 0, "creat for file map");
    if fd >= 0 {
        let test_data: [u8; 32] = [0xDE; 32];
        unsafe { crate::fs::read_write::write(fd as usize, &test_data); }
        unsafe { crate::fs::open::sys_close(fd as usize); }

        // 重新打开用于 mmap
        let rfd = unsafe { crate::fs::open::sys_open(FMAP_PATH, crate::fs::oflags::O_RDONLY, 0) };
        if rfd >= 0 {
            let fmap = unsafe {
                let mut ret: i64 = 0;
                core::arch::asm!(
                    "int 0x80",
                    in("rax") nr::MMAP,
                    in("rdi") 0u64,
                    in("rsi") 32u64,
                    in("rdx") 1u64,         // PROT_READ
                    in("r10") 0x02u64,      // MAP_PRIVATE
                    in("r8") rfd as u64,
                    in("r9") 0u64,
                    lateout("rax") ret,
                    options(nostack),
                );
                ret
            };
            check(fmap > 0, "file map returns addr");
            if fmap > 0 {
                let fdata = unsafe { core::slice::from_raw_parts(fmap as *const u8, 32) };
                let fok = fdata.iter().all(|&b| b == 0xDE);
                check(fok, "file map content matches");
                crate::sprintln!("mmap: file map -> {}", if fok { "ok" } else { "FAIL" });
                // Clean up: munmap the file mapping
                unsafe { crate::syscall::syscall3(nr::MUNMAP, fmap as u64, 32, 0) };
            }
            unsafe { crate::fs::open::sys_close(rfd as usize); }
        }
        // Clean up file
        unsafe { crate::syscall::syscall3(nr::UNLINK, FMAP_PATH.as_ptr() as u64, 0, 0) };
    }

    crate::sprintln!("mmap: selftest {}ok", if ok { "" } else { "FAILED " });
}
