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

/// `sys_uname` 报告的系统信息。对应原版 `include/linux/utsname.h` 里
/// `init_uts_ns` 的字段和 `version.c` 的 `UTS_RELEASE`。
pub const UTS_SYSNAME: &str = "shitix";
pub const UTS_RELEASE: &str = "1.0.9-rust";
pub const UTS_VERSION: &str = "#1";
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

    // 对应原版 start_kernel 的 mem_init(...)
    // SAFETY: 启动早期，中断仍关闭（head.S 未 sti），只调用一次；
    // kernel_end 来自链接脚本，e820::usable 反映真实物理内存。
    let info = unsafe { mm::init(kernel_end, e820::usable()) };

    // 同原版 mem_init 末尾那行 printk("Memory: %luk/%luk available ...")
    cprintln!(Color::LightGreen, Color::Black,
              "Memory: {}k/{}k available ({} kernel pages, {} reserved)",
              info.available / 1024, info.high_memory / 1024,
              info.kernel_pages, info.reserved_pages);

    sprintln!("mm: {}k/{}k available, {} kernel, {} reserved, mem_map at {:#x}",
              info.available / 1024, info.high_memory / 1024,
              info.kernel_pages, info.reserved_pages, info.mem_map_addr);

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

    trap_selftest();
    syscall_selftest();
    sched_selftest();

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
    serial::print("mm: selftest done\n");
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
            syscall::syscall0(nr::OPEN),        // 表里是 ni_syscall → -EINVAL
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
    // SAFETY: 同上；fd=0 不被支持，应返回 -EINVAL。
    let bad_fd = unsafe { syscall::syscall3(nr::WRITE, 0, msg.as_ptr() as u64, 1) };
    kprintln!("syscall: write returned {} (expect {}), bad fd {} -> {}",
              n, msg.len(), bad_fd,
              if n == msg.len() as i64 && bad_fd == -(klib::errno::EINVAL as i64) {
                  "ok"
              } else {
                  "FAIL"
              });

    // SAFETY: 同上。
    unsafe { syscall::syscall0(nr::UNAME) };
    syscall::dump();
    serial::print("syscall: selftest done\n");
}

/// 网络协议栈自检。
/// 测试覆盖：SkBuff、IP 校验和、地址转换、Ethernet、ARP、路由、Socket。
fn net_selftest() {
    net::tests::run_all();
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
fn fs_init_thread(_arg: u64) {
    // 造根文件系统。原版这一步是 rd_load() 从软驱读现成映像，
    // 我们在内存里现造（见 src/fs/minix/mkfs.rs 的模块文档）。
    // SAFETY: ramdisk 已 init，缓冲缓存里还没有本设备的块。
    let layout = unsafe { fs::minix::mkfs(drivers::block::ramdisk::RD_BLOCKS as u32, 512) };
    if layout.is_none() {
        panic!("mkfs.minix failed");
    }

    // 对应原版 start_kernel 末尾的 mount_root()
    // SAFETY: 根设备可读，fs 表已建好，且我们不是 task[0]。
    let mounted = unsafe { fs::mount_root(drivers::block::ramdisk::RAMDISK_DEV, 0) };
    if !mounted {
        panic!("VFS: Unable to mount root");
    }

    fs_selftest();

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

/// 测试用内核线程：自增自己的计数器，睡几个滴答，重复。
///
/// 必须**睡**而不是 yield：task[0]（主自检所在的 idle 任务）只有在没有
/// 别的 Running 任务时才会被调度到，光让出的话两个 worker 会一直
/// 乒乓下去，主线程永远回不来。
fn worker(id: u64) {
    let idx = (id & 1) as usize;
    loop {
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
            && sb_magic == fs::minix::MINIX_SUPER_MAGIC
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
        let mut pos = 0u64;
        let (mut n, mut d, mut dd) = (0, false, false);
        while let Some(e) = fs::minix::dir::readdir(r, pos) {
            let name = &e.name[..e.name_len];
            if name == b"." {
                d = true;
            }
            if name == b".." {
                dd = true;
            }
            n += 1;
            pos = e.offset + 16;
            if n > 8 {
                break;
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
        fs::minix::bitmap::count_free(sb_nr, true)
    };

    // ---- 4. 创建 / 写 / 读回 ----
    const MSG: &[u8] = b"hello from shitix minix fs\n";
    // SAFETY: 同上；open/write/read 都是进程上下文的正常调用。
    let file_ok = unsafe {
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
    };
    kprintln!("fs: creat/write/lseek/read roundtrip -> {}", if file_ok { "ok" } else { "FAIL" });

    // ---- 5. 跨块 + 一级间接块 ----
    // 9KB 需要 9 个块：7 个直接 + 2 个走一级间接。
    const BIG: usize = 9 * 1024;
    // SAFETY: 同上。
    let big_ok = unsafe {
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
            let mut st = fs::stat::Stat::zeroed();
            fs::stat::sys_fstat(fd, &mut st);
            st.st_size
        };
        fs::open::sys_close(fd);
        ok && sz == BIG as u32
    };
    kprintln!("fs: {}KB file (7 direct + indirect) -> {}", BIG / 1024, if big_ok { "ok" } else { "FAIL" });

    // ---- 6. mkdir / unlink / rmdir 与位图回收 ----
    // SAFETY: 同上。
    let dir_ok = unsafe {
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
            fs::minix::bitmap::count_free(sb_nr, true)
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
    };
    kprintln!("fs: mkdir/rmdir/unlink + zone reclaim -> {}", if dir_ok { "ok" } else { "FAIL" });

    // ---- 字符设备：/dev/zero 与 /dev/null ----
    // SAFETY: 同上。
    let chr_ok = unsafe {
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
