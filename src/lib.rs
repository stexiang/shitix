//! shitix 内核入口
//!
//! 由 boot/head.S 在 long mode 下通过 `call start_kernel` 进入。
//! 对应 linux-1.0.9 的 init/main.c 里的 start_kernel()。

#![no_std]
#![no_main]

pub mod console;
pub mod e820;
pub mod mm;
pub mod serial;

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

    cprintln!(Color::Yellow, Color::Black, "shitix: boot ok, idling.");
    serial::print("shitix: boot ok\n");

    // 测试脚本靠这个标记判断启动成功
    serial::print("SHITIX_BOOT_OK\n");

    halt_loop();
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

fn halt_loop() -> ! {
    loop {
        // SAFETY: hlt 在 CPL=0 下合法，仅让 CPU 等待下一个中断，不访问内存。
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // 红底白字，和正常输出区分开
    console::set_color(Color::White, Color::Red);
    println!();
    print!("KERNEL PANIC: ");
    if let Some(msg) = info.message().as_str() {
        print!("{}", msg);
    }
    if let Some(loc) = info.location() {
        print!(" at {}:{}", loc.file(), loc.line());
    }
    println!();

    serial::print("SHITIX_PANIC\n");
    halt_loop();
}
