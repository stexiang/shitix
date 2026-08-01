//! 内存管理子系统。
//!
//! 对应 linux-1.0.9 的 `mm/` 目录：
//!
//! | 本模块 | 原版 |
//! |---|---|
//! | [`page`] | `include/linux/page.h` 的常量与宏 |
//! | [`page_alloc`] | `mm/memory.c` 的 `mem_init()` + `mm/swap.c` 的 `__get_free_page`/`free_page` |
//! | [`kmalloc`] | `mm/kmalloc.c` 全部 |
//! | [`paging`] | `mm/memory.c` 的 `put_page`/`remap_page_range`/`invalidate` |
//!
//! 尚未移植的部分：`mm/swap.c` 的换页（缺块设备）、`mm/mmap.c` 的
//! `vm_area_struct` 管理（缺进程）、`mm/vmalloc.c`（依赖前两者）。
//! 这几个要等进程和块设备到位后再做。

pub mod kmalloc;
pub mod page;
pub mod page_alloc;
pub mod paging;

pub use kmalloc::{kfree, kmalloc, kzalloc};
pub use page::{PAGE_SIZE, page_align};
pub use page_alloc::{MemInfo, free_page, get_free_page, nr_free_pages};

/// 初始化整个 MM 子系统。对应原版 `start_kernel()` 里那句
/// `mem_init(low_memory_start, memory_start, memory_end)`。
///
/// # Safety
/// 启动早期、中断关闭时调用一次。`kernel_end` 必须是内核镜像末尾的物理地址，
/// `regions` 必须真实反映可用物理内存。
pub unsafe fn init<I>(kernel_end: usize, regions: I) -> MemInfo
where
    I: Iterator<Item = (u64, u64)> + Clone,
{
    // SAFETY: 契约直接转交给 page_alloc::init。
    unsafe { page_alloc::init(kernel_end, regions) }
}
