//! 物理页帧分配器。
//!
//! 对应 linux-1.0.9 的 `mm/memory.c` 中的 `mem_init()`、以及 `mm/swap.c` 中的
//! `__get_free_page()` / `free_page()`（`REMOVE_FROM_MEM_QUEUE` /
//! `add_mem_queue` 两个宏）。
//!
//! 保留的原版机制：
//!   - `mem_map`：每个物理页一个 `u16` 引用计数，`MAP_PAGE_RESERVED` 标记保留页
//!   - 空闲页单链表，链表指针直接存在空闲页的头 8 字节里（原版是头 4 字节），
//!     所以分配器自身不需要额外内存
//!   - `free_page()` 递减引用计数，减到 0 才真正回收；保留页直接忽略
//!
//! 与原版的差异：
//!   - 不实现 `secondary_page_list`（原版给中断上下文的 20 页备用池）与
//!     `try_to_free_page()` 换页，因为还没有 swap 和块设备
//!   - 空闲页范围由 E820 决定，而不是原版 `mem_init(start_low_mem, start_mem, end_mem)`
//!     里那套「低端内存 + BIOS 洞」的硬编码假设

use super::page::{PAGE_SHIFT, PAGE_SIZE, map_nr, page_align};
use core::sync::atomic::{AtomicUsize, Ordering};

/// 保留页标记。同原版 `MAP_PAGE_RESERVED`。
const MAP_PAGE_RESERVED: u16 = 0x8000;

/// 空闲链表末端哨兵。物理地址 0 永远是保留页（BIOS IVT），不会是合法空闲页。
const NIL: usize = 0;

/// `mem_map` 基址与长度。对应原版的全局 `mem_map` 与 `high_memory >> PAGE_SHIFT`。
static mut MEM_MAP: *mut u16 = core::ptr::null_mut();
static mut MEM_MAP_LEN: usize = 0;

/// 空闲页的完好性 cookie，写在链表指针后面那 8 字节（仅 debug 构建校验）。
///
/// 空闲链表是**穿过页内容本身**的（同原版 `*(unsigned long *)tmp =
/// free_page_list`），所以任何「已经 free 掉却还在写」的旧主人都会把链
/// 打断，而故障要等到后面某次 `get_free_page` 摘链时才暴露，症状离原因
/// 极远（典型表现：链头变成物理页 0，于是读到 BIOS IVT 的
/// `0xf000ff53`，再被当成下标/魔数用）。cookie 让这种 use-after-free
/// 在摘链的那一刻就报出坏页地址。
const FREE_COOKIE: usize = 0xF0F0_C0DE_F0F0_C0DE;

/// 空闲页链表头。同原版 `free_page_list`。
///
/// 只能通过 [`free_list_head`] / [`set_free_list_head`] 访问。直接读写这个
/// `static mut` 会被编译器缓存：`get_free_page_raw` 被内联进
/// `ramdisk::init` 那种分配循环后，LLVM 会把上一轮算出的链头沿用到下一轮，
/// 而中断/其它路径（`free_page`）也在改它。实测症状是
/// `get_free_page` 返回 0（上层报 out of memory）而链头其实是
/// `0xffdf000` 这种完好的值，还剩 65213 页空闲。原版 C 里
/// `free_page_list` 的每次访问都是真实的内存访问，没有这个问题。
static mut FREE_PAGE_LIST: usize = NIL;

/// 分配器全局量两侧的哨兵。
///
/// `FREE_PAGE_LIST` / `NR_FREE_PAGES` / `HIGH_MEMORY` 在 `.bss` 里紧挨着
/// 别人的缓冲（实测前面就是自检用的两个 1KB 数组）。谁越界写过来，这
/// 三个量会**一起**变成 0，表现为 `get_free_page` 静默返回 0
/// （上层「out of memory」），而链表本身从未被判定为损坏——因为链头和
/// 计数同时归零，所有一致性检查都通过。哨兵能把「越界写」和「链表逻辑
/// 错」区分开。
static mut GUARD_BEFORE: [u64; 4] = [0; 4];
/// 见 [`GUARD_BEFORE`]。
static mut GUARD_AFTER: [u64; 4] = [0; 4];
/// 哨兵图案。
const GUARD_PAT: u64 = 0xA110_C8_DEAD_BEEF;

/// 打上哨兵图案。零初始化是故意的：非零初值会把数组塞进 `.data`，
/// 就挨不到 `.bss` 里的这几个全局量了。
///
/// # Safety
/// 启动期调用一次。
unsafe fn stamp_guards() {
    // SAFETY: 只写两个静态数组。
    unsafe {
        for p in [core::ptr::addr_of_mut!(GUARD_BEFORE), core::ptr::addr_of_mut!(GUARD_AFTER)] {
            for i in 0..4 {
                core::ptr::write_volatile((p as *mut u64).add(i), GUARD_PAT);
            }
        }
    }
}

/// 检查分配器全局量两侧的哨兵。
///
/// # Safety
/// 只读两个静态数组；任何时候可调用。
pub unsafe fn check_guards(tag: &str) {
    // SAFETY: 只按值读两个已初始化的静态数组。
    unsafe {
        for (name, p) in [
            ("before", core::ptr::addr_of!(GUARD_BEFORE)),
            ("after", core::ptr::addr_of!(GUARD_AFTER)),
        ] {
            for i in 0..4 {
                let v = core::ptr::read_volatile((p as *const u64).add(i));
                assert!(
                    v == GUARD_PAT,
                    "page_alloc guard {} word {} smashed: {:#x} (at {})",
                    name, i, v, tag
                );
            }
        }
    }
}

/// 统计量。同原版 `nr_free_pages`；用 atomic 只是为了以后开中断后仍可读。
static NR_FREE_PAGES: AtomicUsize = AtomicUsize::new(0);
/// `high_memory`：可管理物理内存的上界。
static mut HIGH_MEMORY: usize = 0;

/// `mem_init()` 报告的内存统计，供 `start_kernel` 打印（同原版那行 printk）。
#[derive(Copy, Clone, Default)]
pub struct MemInfo {
    /// 可用（已进空闲链表）字节数
    pub available: usize,
    /// 物理内存上界
    pub high_memory: usize,
    /// 内核代码+数据占用的页数
    pub kernel_pages: usize,
    /// 保留页数（BIOS 洞、E820 非 usable 区、mem_map 自身）
    pub reserved_pages: usize,
    /// `mem_map` 表体所在物理地址，便于启动期核对布局
    pub mem_map_addr: usize,
}

/// 取 `mem_map` 的可变切片。
///
/// # Safety
/// 调用者必须保证 [`init`] 已完成，且当前没有其他执行流持有该切片。
/// `mem_map[nr]` 的裸指针。
///
/// `mem_map()` 返回 `&'static mut [u16]`，带 LLVM `noalias`；分配/释放路径
/// 在同一个函数里既动空闲链表又改引用计数，两条重叠的 `&mut` 会让编译器
/// 丢弃或复用其中一条上的访问。实测症状：`get_free_page` 静默返回 0
/// （上层报 out of memory）而链表、计数、护栏全都完好。
///
/// # Safety
/// `init` 已跑过且 `nr < MEM_MAP_LEN`（本函数会断言）。
#[inline]
#[track_caller]
unsafe fn mm_ent(nr: usize) -> *mut u16 {
    // SAFETY: 只读两个静态标量。
    let (base, len) = unsafe {
        (
            core::ptr::read_volatile(core::ptr::addr_of!(MEM_MAP)),
            core::ptr::read_volatile(core::ptr::addr_of!(MEM_MAP_LEN)),
        )
    };
    assert!(!base.is_null() && nr < len, "mm_ent({}): out of range (len={})", nr, len);
    // SAFETY: 上面已校验非空且下标在界内。
    unsafe { base.add(nr) }
}

unsafe fn mem_map() -> &'static mut [u16] {
    // SAFETY: init() 把 MEM_MAP 指向一块位于可用 RAM 内、长度为 MEM_MAP_LEN 的
    // u16 数组，并已全部写入初值；该区域随后被标记为保留页，不会被分配走。
    // 由调用者契约保证无并发访问。
    unsafe { core::slice::from_raw_parts_mut(MEM_MAP, MEM_MAP_LEN) }
}

/// 初始化页帧分配器。对应原版 `mem_init()`。
///
/// `kernel_end` 是内核镜像结束的物理地址（链接脚本的 `_kernel_end`）；
/// `regions` 是 E820 报告的可用区间迭代器，`(base, len)` 单位为字节。
///
/// # Safety
/// 只能在启动早期、中断关闭的情况下调用一次。`regions` 必须真实反映
/// 物理内存布局，否则分配器会把不存在或已被占用的内存派发出去。
pub unsafe fn init<I>(kernel_end: usize, regions: I) -> MemInfo
where
    I: Iterator<Item = (u64, u64)> + Clone,
{
    // 先求物理内存上界。原版由 mem_init 的 end_mem 参数给出，这里从 E820 推导。
    let mut high = 0usize;
    for (base, len) in regions.clone() {
        let end = base.saturating_add(len);
        // 只管理 64 位地址空间里我们已恒等映射的低 1GB
        let end = end.min(LOW_MAPPED_LIMIT as u64) as usize;
        if end > high {
            high = end;
        }
    }
    let high = super::page::page_base(high);

    // mem_map 紧跟在内核镜像之后，长度覆盖 0..high 的每一页。同原版：
    //   mem_map = (unsigned short *) start_mem; p = mem_map + MAP_NR(end_mem);
    let nr_pages = map_nr(high);
    // mem_map 必须落在 1MB 之上：内核镜像本身在 0x10000，若紧贴其后放表，
    // 表体会盖住 0x90000 的参数区（页表在 0x4000，见 setup.S）。
    let map_addr = page_align(kernel_end).max(MIN_USABLE_PHYS);
    let map_bytes = nr_pages * core::mem::size_of::<u16>();

    // SAFETY: map_addr 在内核镜像之后、high 之下，属于恒等映射的低端 RAM；
    // 写入 nr_pages 个 u16 不会越过 high（下面 map_end 会被算进保留区）。
    unsafe {
        MEM_MAP = map_addr as *mut u16;
        MEM_MAP_LEN = nr_pages;
        HIGH_MEMORY = high;
        // 原版：while (p > mem_map) *--p = MAP_PAGE_RESERVED;
        // 默认全部标记为保留，随后只把确认可用的页放开。
        core::ptr::write_bytes(MEM_MAP as *mut u8, 0, map_bytes);
        for e in mem_map().iter_mut() {
            *e = MAP_PAGE_RESERVED;
        }
    }

    // 空闲页的下界：既要越过 mem_map 自身，也要越过低 1MB 的启动期结构。
    let map_end = page_align(map_addr + map_bytes).max(MIN_USABLE_PHYS);

    // 把 E820 的 usable 区间中、位于 map_end 之上的页放开（清掉 RESERVED）。
    // 对应原版那两个 while 循环（放开低端内存和 start_mem..end_mem）。
    // SAFETY: init 独占执行，mem_map 已初始化完毕。
    let map = unsafe { mem_map() };
    for (base, len) in regions {
        let start = page_align(base as usize).max(map_end);
        let end = super::page::page_base((base.saturating_add(len)) as usize).min(high);
        let mut addr = start;
        while addr < end {
            map[map_nr(addr)] = 0;
            addr += PAGE_SIZE;
        }
    }

    // 建空闲链表。对应原版 mem_init 末尾那个 for 循环。
    let mut reserved = 0usize;
    let mut free = 0usize;
    // SAFETY: 仍处于 init 的独占阶段。
    unsafe {
        stamp_guards();
        set_free_list_head(NIL);
    }
    let mut addr = 0usize;
    while addr < high {
        if map[map_nr(addr)] != 0 {
            reserved += 1;
        } else {
            // 链表指针存在空闲页自身的头 8 字节里。同原版
            //   *(unsigned long *) tmp = free_page_list; free_page_list = tmp;
            // SAFETY: addr 是一整页可用 RAM 且已被恒等映射，写它的头 8 字节
            // 不影响任何其他数据——这一页当前不属于任何使用者。
            unsafe {
                push_free(addr);
            }
            free += 1;
        }
        addr += PAGE_SIZE;
    }
    NR_FREE_PAGES.store(free, Ordering::Relaxed);

    MemInfo {
        available: free << PAGE_SHIFT,
        high_memory: high,
        kernel_pages: map_nr(map_end),
        reserved_pages: reserved,
        mem_map_addr: map_addr,
    }
}

/// 只管理已恒等映射的低 1GB（setup.S 用 2MB 大页映射到这里）。
const LOW_MAPPED_LIMIT: usize = 1 << 30;

/// 可分配内存的下界：低 1MB 整体保留，绝不进空闲链表。
///
/// 这块地方塞满了启动期结构，E820 却把其中大部分报成 usable：
///   0x09000 bootsect 自搬后的落点、0x90000 机器参数区、0x9E000 E820 数组、
///   0x04000-0x06FFF 四级页表、0xA0000-0xFFFFF BIOS/VGA 洞。
/// 原版 `mem_init` 靠 `start_low_mem` 参数和 `0xA0000` 硬编码来绕开它们；
/// 我们没有等价参数，索引干脆整个低 1MB 都不碰。
const MIN_USABLE_PHYS: usize = 0x10_0000;

/// 取一页物理内存，不清零。对应原版 `__get_free_page()`。
///
/// 返回物理地址，失败返回 0（同原版用 0 表示失败）。
pub fn get_free_page_raw() -> usize {
    // 原版 `__get_free_page` 里的 `REMOVE_FROM_MEM_QUEUE` 宏是
    // `cli(); ... restore_flags(flags);`——摘链、写 `nr_free_pages`、
    // 改 `mem_map` 引用计数这三步必须是一个原子单位。
    //
    // 不关中断的后果：空闲链表的头 8 字节既是链接指针又是页内容，
    // 「读 FREE_PAGE_LIST」和「把它的 next 写回 FREE_PAGE_LIST」之间被
    // 中断打断、而中断里也走分配/释放路径的话，同一页会被派给两个使用者。
    // 两个缓冲头共用一个数据页 / ramdisk 的某块与缓冲重叠，都会由此产生，
    // 症状是随机的文件系统损坏（readdir 丢项、位图下标里出现低端内存的
    // `0xf000ff53`），离原因非常远。
    // SAFETY: 全程关中断，独占空闲链表与 mem_map。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let page = get_free_page_locked();
        crate::irq::restore_flags(flags);
        page
    }
}

/// 读链头。见 [`FREE_PAGE_LIST`] 的说明：必须是 volatile。
///
/// # Safety
/// 调用者应已关中断（否则读到的值可能立刻过期）。
#[inline]
unsafe fn free_list_head() -> usize {
    // SAFETY: 读一个已初始化的静态 usize。
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(FREE_PAGE_LIST)) }
}

/// 写链头。见 [`free_list_head`]。
///
/// # Safety
/// 必须在关中断状态下调用。
#[inline]
unsafe fn set_free_list_head(v: usize) {
    // SAFETY: 写一个静态 usize；契约保证独占。
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(FREE_PAGE_LIST), v) }
}

/// 把一页压回空闲链表，顺带写下 [`FREE_COOKIE`]。
///
/// # Safety
/// 必须在关中断状态下调用，且 `base` 是页对齐、已无使用者的一整页。
unsafe fn push_free(base: usize) {
    // SAFETY: 契约保证这一页当前不属于任何使用者，可复用头 16 字节。
    unsafe {
        if cfg!(debug_assertions) {
            check_guards("push_free");
        }
        // 重复入链检查。链表是单链且穿过页内容，同一页进两次会让它的
        // 头 8 字节只能记住后一个 next，于是前一个持有者的 next 仍指向
        // 它 —— 链上出现环，`get_free_page` 就会把同一页派给两个主人
        // （症状：两个缓冲头 b_data 重叠、写一个块改到另一个块）。
        // 页对齐、上下界也一起查：坏地址进链后要等很多次分配才暴露。
        let high = *core::ptr::addr_of!(HIGH_MEMORY);
        assert!(
            base & (PAGE_SIZE - 1) == 0 && base >= 0x10_0000 && base < high,
            "push_free: bad page {:#x} (high_memory={:#x})",
            base, high
        );
        if cfg!(debug_assertions) {
            let cookie = core::ptr::read_volatile((base + 8) as *const usize);
            assert!(
                cookie != FREE_COOKIE,
                "page {:#x} freed twice (already on free list)",
                base
            );
        }
        let head = free_list_head();
        core::ptr::write_volatile(base as *mut usize, head);
        core::ptr::write_volatile((base + 8) as *mut usize, FREE_COOKIE);
        // next 的冗余副本。链表指针存在页内容里，一旦有旧主人在 free
        // 之后继续写，链就断在这里；两份副本不一致就能立刻说出「被写坏
        // 的宽度」（只坏 8 字节 = 单个指针写；连 cookie 一起坏 = 大块
        // memset/memcpy）。
        core::ptr::write_volatile((base + 16) as *mut usize, !head);
        set_free_list_head(base);
    }
}

/// [`get_free_page_raw`] 的临界区主体。
///
/// # Safety
/// 必须在关中断状态下调用。
unsafe fn get_free_page_locked() -> usize {
    // SAFETY: 契约保证已关中断。
    unsafe {
        if cfg!(debug_assertions) {
            check_guards("get_free_page");
        }
        let page = free_list_head();
        if page == NIL {
            // 链空但计数非零 = 链被截断了（有人把某个空闲页的 next 写成
            // 0）。原版这里直接返回 0，上层报「out of memory」——而实际
            // 上还有几万页空闲，故障看起来完全不像内存损坏。
            // 计数与链表必须同时为空。这个检查放在临界区**内部**：
            // 放到调用侧（restore_flags 之后）读计数是错的——中断已经
            // 开了，别的路径可能刚 free 过页，报出来的数字与判断时刻不
            // 对应，看起来像「返回 0 但还剩几万页空闲」的自相矛盾。
            let n = NR_FREE_PAGES.load(Ordering::Relaxed);
            assert!(
                n == 0,
                "free page list empty but nr_free_pages={} (list was truncated)",
                n
            );
            return 0;
        }
        // 原版 REMOVE_FROM_MEM_QUEUE 的健全性检查：必须页对齐且低于 high_memory
        // 低 1MB 永远是保留页，绝不该出现在链上。原版的
        // REMOVE_FROM_MEM_QUEUE 只查了上界，少了下界就会让「链头被写成
        // 0 或某个低端小整数」这种损坏悄悄通过，返回值 0 又刚好被上层
        // 当成 OOM，于是 panic 在离现场很远的地方。
        let high = core::ptr::read_volatile(core::ptr::addr_of!(HIGH_MEMORY));
        if page & (PAGE_SIZE - 1) != 0 || page >= high || page < 0x10_0000 {
            // 链表被写坏了，丢弃整条链而不是继续用（原版此时打印并把队列清空）
            // 原版只是静默丢链，但那会让故障在很远的地方以「out of
            // memory」的形式出现（明明还有几万页空闲）。既然链头已经是
            // 垃圾，直接报出来：坏值本身就指明了是谁写进去的。
            set_free_list_head(NIL);
            let n = NR_FREE_PAGES.swap(0, Ordering::Relaxed);
            panic!(
                "free page list corrupt: head={:#x} (high_memory={:#x}, {} pages were free)",
                page, high, n
            );
        }
        // SAFETY: page 是页对齐、< HIGH_MEMORY 的空闲页，头 8 字节存着下一项。
        let next = core::ptr::read_volatile(page as *const usize);
        if cfg!(debug_assertions) {
            let mirror = core::ptr::read_volatile((page + 16) as *const usize);
            assert!(
                mirror == !next,
                "free page {:#x}: next={:#x} but mirror={:#x} (link overwritten)",
                page, next, mirror
            );
            let cookie = core::ptr::read_volatile((page + 8) as *const usize);
            assert!(
                cookie == FREE_COOKIE,
                "page {:#x} on free list was written after being freed \
                 (cookie {:#x} != {:#x}, next={:#x})",
                page, cookie, FREE_COOKIE, next
            );
        }
        set_free_list_head(next);
        NR_FREE_PAGES.fetch_sub(1, Ordering::Relaxed);
        let nr = map_nr(page);
        // 在链上的页引用计数必须是 0。不是 0 说明这一页已经有主人却又
        // 出现在空闲链上，接着派出去就是两个主人共用一页。
        debug_assert!(
            *mm_ent(nr) == 0,
            "get_free_page: page {:#x} on free list but mem_map={:#x}",
            page, *mm_ent(nr)
        );
        // 抹掉 cookie，避免这一页被派出去后原封不动地拿回来时
        // 「重复 free」检查漏判。
        core::ptr::write_volatile((page + 8) as *mut usize, 0);
        *mm_ent(nr) = 1;
        page
    }
}

/// 取一页并清零。对应原版 `mm.h` 里的 inline `get_free_page()`
/// （它在 `__get_free_page` 之后做 `rep stosl`）。
pub fn get_free_page() -> usize {
    let page = get_free_page_raw();
    // 返回 0 只有「真的没内存」一种合法解释。计数还剩几万页却拿到 0，
    // 说明链表或返回值出了问题——上层看到的是「out of memory」，离原因
    // 极远（实测：ramdisk init 在第 19/21/26 页突然 OOM，而空闲页 65214）。
    if page != 0 {
        // SAFETY: page 是刚从空闲链表摘下的一整页，独占持有，清零安全。
        unsafe { core::ptr::write_bytes(page as *mut u8, 0, PAGE_SIZE) }
    }
    page
}

/// 释放一页。对应原版 `free_page()`：递减引用计数，减到 0 才回收；
/// 保留页与越界地址直接忽略。
pub fn free_page(addr: usize) {
    // 同 [`get_free_page_raw`]：原版这里也是 cli/restore_flags 包住整段。
    // SAFETY: 全程关中断，独占 mem_map 与空闲链表。
    unsafe {
        let flags = crate::irq::local_irq_save();
        free_page_locked(addr);
        crate::irq::restore_flags(flags);
    }
}

/// [`free_page`] 的临界区主体。
///
/// # Safety
/// 必须在关中断状态下调用。
unsafe fn free_page_locked(addr: usize) {
    // SAFETY: 契约保证已关中断。
    unsafe {
        if addr >= HIGH_MEMORY || MEM_MAP.is_null() {
            return;
        }
        let nr = map_nr(addr);
        // 用 `mm_ent` 而不是 `let map = mem_map()`：那条 `&mut [u16]` 会一直
        // 活到函数结束，跨过下面的 `push_free`——而 `push_free` 要写这一页
        // 的头 16 字节，正是 `mem_map` 引用计数所描述的那块内存。两条重叠
        // 的 `&mut` 带 noalias，编译器可以据此丢弃/重排其中一条上的写。
        let entry = *mm_ent(nr);
        if entry == 0 {
            // 原版这里 printk("Trying to free free memory ...")
            return;
        }
        if entry & MAP_PAGE_RESERVED != 0 {
            return;
        }
        *mm_ent(nr) = entry - 1;
        if *mm_ent(nr) == 0 {
            let base = super::page::page_base(addr);
            // SAFETY: 引用计数归零，这一页已无使用者，可复用其头 8 字节做链表指针。
            push_free(base);
            NR_FREE_PAGES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 增加一页的引用计数，用于共享页（原版 `mem_map[MAP_NR(x)]++`，
/// 见 `copy_page_tables()`）。保留页不计数。
pub fn get_page(addr: usize) {
    // SAFETY: 关中断后独占 mem_map（读—改—写引用计数不是原子的）。
    unsafe {
        let flags = crate::irq::local_irq_save();
        if addr < HIGH_MEMORY && !MEM_MAP.is_null() {
            // 同 [`free_page_locked`]：走裸指针，不留活着的 `&mut [u16]`。
            let nr = map_nr(addr);
            if *mm_ent(nr) & MAP_PAGE_RESERVED == 0 {
                *mm_ent(nr) += 1;
            }
        }
        crate::irq::restore_flags(flags);
    }
}

/// 当前空闲页数。同原版 `nr_free_pages`。
/// 可管理物理内存的上界。对应原版的全局 `high_memory`。
/// `drivers/char/mem.c` 的 `/dev/mem` 用它做地址范围检查。
pub fn high_memory() -> usize {
    // SAFETY: 只读一个 usize，`init` 之后不再变。
    unsafe { *core::ptr::addr_of!(HIGH_MEMORY) }
}

pub fn nr_free_pages() -> usize {
    NR_FREE_PAGES.load(Ordering::Relaxed)
}

/// 某页的引用计数，`None` 表示越界。调试与自检用。
pub fn page_count(addr: usize) -> Option<u16> {
    // SAFETY: 只读 mem_map，早期单线程。
    unsafe {
        if addr >= HIGH_MEMORY || MEM_MAP.is_null() {
            return None;
        }
        Some(*mm_ent(map_nr(addr)))
    }
}
