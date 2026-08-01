//! 分页相关的基本常量与换算。
//!
//! 对应 linux-1.0.9 的 `include/linux/page.h`（`PAGE_SHIFT` / `PAGE_SIZE` /
//! `PAGE_MASK` / `PAGE_ALIGN`）以及 `include/linux/mm.h` 里的 `MAP_NR`。
//!
//! 与原版的差异：原版是 32 位两级分页，页目录项/页表项都是 `unsigned long`(4B)；
//! 这里目标是 x86_64 四级分页，条目 8 字节。页大小仍是 4KB，所以本文件的
//! 常量与原版一致，只有 `PTRS_PER_PAGE` 因指针变宽而从 1024 变成 512。

/// 页大小的位移量。同原版 `PAGE_SHIFT`。
pub const PAGE_SHIFT: usize = 12;
/// 页大小 4096。同原版 `PAGE_SIZE`。
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
/// 抹掉页内偏移的掩码。同原版 `PAGE_MASK`。
pub const PAGE_MASK: usize = !(PAGE_SIZE - 1);
/// 一页能放下多少个 64 位表项（原版 32 位下是 1024）。
pub const PTRS_PER_PAGE: usize = PAGE_SIZE / core::mem::size_of::<u64>();

/// 向上取整到页边界。同原版 `PAGE_ALIGN`。
#[inline]
pub const fn page_align(addr: usize) -> usize {
    (addr + PAGE_SIZE - 1) & PAGE_MASK
}

/// 向下取整到页边界。
#[inline]
pub const fn page_base(addr: usize) -> usize {
    addr & PAGE_MASK
}

/// 物理地址 → `mem_map` 下标。同原版 `MAP_NR(addr)`。
#[inline]
pub const fn map_nr(addr: usize) -> usize {
    addr >> PAGE_SHIFT
}
