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

/// 空闲页链表头。同原版 `free_page_list`。
static mut FREE_PAGE_LIST: usize = NIL;

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
    // 表体会盖住 0x70000 的页表和 0x90000 的参数区。
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
        FREE_PAGE_LIST = NIL;
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
                core::ptr::write_volatile(addr as *mut usize, FREE_PAGE_LIST);
                FREE_PAGE_LIST = addr;
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
///   0x70000-0x72FFF 四级页表、0xA0000-0xFFFFF BIOS/VGA 洞。
/// 原版 `mem_init` 靠 `start_low_mem` 参数和 `0xA0000` 硬编码来绕开它们；
/// 我们没有等价参数，索引干脆整个低 1MB 都不碰。
const MIN_USABLE_PHYS: usize = 0x10_0000;

/// 取一页物理内存，不清零。对应原版 `__get_free_page()`。
///
/// 返回物理地址，失败返回 0（同原版用 0 表示失败）。
pub fn get_free_page_raw() -> usize {
    // SAFETY: 内核早期单线程；开中断后需要改为在 cli 保护下操作（原版正是
    // 用 cli/restore_flags 包住 REMOVE_FROM_MEM_QUEUE）。
    unsafe {
        let page = FREE_PAGE_LIST;
        if page == NIL {
            return 0;
        }
        // 原版 REMOVE_FROM_MEM_QUEUE 的健全性检查：必须页对齐且低于 high_memory
        if page & (PAGE_SIZE - 1) != 0 || page >= HIGH_MEMORY {
            // 链表被写坏了，丢弃整条链而不是继续用（原版此时打印并把队列清空）
            FREE_PAGE_LIST = NIL;
            NR_FREE_PAGES.store(0, Ordering::Relaxed);
            return 0;
        }
        // SAFETY: page 是页对齐、< HIGH_MEMORY 的空闲页，头 8 字节存着下一项。
        FREE_PAGE_LIST = core::ptr::read_volatile(page as *const usize);
        NR_FREE_PAGES.fetch_sub(1, Ordering::Relaxed);
        mem_map()[map_nr(page)] = 1;
        page
    }
}

/// 取一页并清零。对应原版 `mm.h` 里的 inline `get_free_page()`
/// （它在 `__get_free_page` 之后做 `rep stosl`）。
pub fn get_free_page() -> usize {
    let page = get_free_page_raw();
    if page != 0 {
        // SAFETY: page 是刚从空闲链表摘下的一整页，独占持有，清零安全。
        unsafe { core::ptr::write_bytes(page as *mut u8, 0, PAGE_SIZE) }
    }
    page
}

/// 释放一页。对应原版 `free_page()`：递减引用计数，减到 0 才回收；
/// 保留页与越界地址直接忽略。
pub fn free_page(addr: usize) {
    // SAFETY: 同 get_free_page_raw，早期单线程独占 mem_map 与链表。
    unsafe {
        if addr >= HIGH_MEMORY || MEM_MAP.is_null() {
            return;
        }
        let nr = map_nr(addr);
        let map = mem_map();
        let entry = map[nr];
        if entry == 0 {
            // 原版这里 printk("Trying to free free memory ...")
            return;
        }
        if entry & MAP_PAGE_RESERVED != 0 {
            return;
        }
        map[nr] = entry - 1;
        if map[nr] == 0 {
            let base = super::page::page_base(addr);
            // SAFETY: 引用计数归零，这一页已无使用者，可复用其头 8 字节做链表指针。
            core::ptr::write_volatile(base as *mut usize, FREE_PAGE_LIST);
            FREE_PAGE_LIST = base;
            NR_FREE_PAGES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 增加一页的引用计数，用于共享页（原版 `mem_map[MAP_NR(x)]++`，
/// 见 `copy_page_tables()`）。保留页不计数。
pub fn get_page(addr: usize) {
    // SAFETY: 同上。
    unsafe {
        if addr >= HIGH_MEMORY || MEM_MAP.is_null() {
            return;
        }
        let map = mem_map();
        let nr = map_nr(addr);
        if map[nr] & MAP_PAGE_RESERVED == 0 {
            map[nr] += 1;
        }
    }
}

/// 当前空闲页数。同原版 `nr_free_pages`。
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
        Some(mem_map()[map_nr(addr)])
    }
}
