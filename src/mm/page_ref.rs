//! 页面引用计数管理
//!
//! 用于实现 COW (Copy-On-Write) 机制。当多个进程共享同一物理页时，
//! 使用引用计数跟踪共享情况，只有当引用计数降到 1 时才能进行写时复制。

use core::sync::atomic::{AtomicU32, Ordering};

/// 引用计数表的表体与长度，由 [`attach`] 在 `page_alloc::init` 里填好。
///
/// 表体不是静态数组：按最大可管理内存静态开表需要 256KB BSS，`_kernel_end`
/// 会顶过 0x90000 盖掉 setup.S 的机器参数区（`kernel.ld` 的 ASSERT 会拦住）。
/// 所以和 `mem_map` 一样按实际物理内存量在启动期划出来。
static mut REF_BASE: *mut AtomicU32 = core::ptr::null_mut();
static mut REF_LEN: usize = 0;

/// 把引用计数表挂到 `base`，长度 `len` 个条目。
///
/// # Safety
/// 只能在启动早期由 `page_alloc::init` 调用一次。`base` 必须指向一块已恒等
/// 映射、长度不小于 `len * 4` 字节、已清零且随后被标记为保留页的内存。
pub unsafe fn attach(base: usize, len: usize) {
    unsafe {
        REF_BASE = base as *mut AtomicU32;
        REF_LEN = len;
    }
}

/// 取 `pfn` 对应的计数槽；表未挂上或下标越界时返回 `None`。
///
/// 越界不 panic：调用方大多在 COW 路径上传入来自页表的 pfn，超出受管内存
/// 范围（如 MMIO）的页本来就不参与引用计数，按“无槽位”处理即可。
#[inline]
fn slot(pfn: usize) -> Option<&'static AtomicU32> {
    // SAFETY: 只读两个静态标量；attach 之后 REF_BASE 在 REF_LEN 范围内始终有效。
    unsafe {
        let (base, len) = (
            core::ptr::read_volatile(core::ptr::addr_of!(REF_BASE)),
            core::ptr::read_volatile(core::ptr::addr_of!(REF_LEN)),
        );
        if base.is_null() || pfn >= len {
            return None;
        }
        Some(&*base.add(pfn))
    }
}

/// 报告表的规模，供启动期打印核对布局。
pub fn table_len() -> usize {
    // SAFETY: 只读一个静态标量。
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(REF_LEN)) }
}

/// 获取页框的引用计数
#[inline]
pub fn page_ref_count(pfn: usize) -> u32 {
    match slot(pfn) {
        Some(s) => s.load(Ordering::Relaxed) & 0xFFFF,
        None => 0,
    }
}

/// 增加页框引用计数
/// 返回新的引用计数
#[inline]
pub fn page_ref_inc(pfn: usize) -> u32 {
    match slot(pfn) {
        Some(s) => (s.fetch_add(1, Ordering::Relaxed) + 1) & 0xFFFF,
        None => 0,
    }
}

/// 减少页框引用计数
/// 返回新的引用计数
#[inline]
pub fn page_ref_dec(pfn: usize) -> u32 {
    match slot(pfn) {
        Some(s) => (s.fetch_sub(1, Ordering::Relaxed).wrapping_sub(1)) & 0xFFFF,
        None => 0,
    }
}

/// 设置页框引用计数
#[inline]
pub fn page_ref_set(pfn: usize, count: u32) {
    if let Some(s) = slot(pfn) {
        // 只改低 16 位的计数，保留高位的 COW 标记
        let cow = s.load(Ordering::Relaxed) & 0x8000_0000;
        s.store((count & 0xFFFF) | cow, Ordering::Relaxed);
    }
}

/// 获取页框的 COW 标记
#[inline]
pub fn page_is_cow(pfn: usize) -> bool {
    match slot(pfn) {
        Some(s) => (s.load(Ordering::Relaxed) & COW_FLAG) != 0,
        None => false,
    }
}

/// 设置页框的 COW 标记
#[inline]
pub fn page_set_cow(pfn: usize, cow: bool) {
    if let Some(s) = slot(pfn) {
        if cow {
            s.fetch_or(COW_FLAG, Ordering::Relaxed);
        } else {
            s.fetch_and(!COW_FLAG, Ordering::Relaxed);
        }
    }
}

/// COW 标记位。放在 bit 31 而不是 bit 15：计数字段是低 16 位（`& 0xFFFF`），
/// bit 15 会和计数重叠，引用计数一旦到 32768 就会被误读成 COW 页。
const COW_FLAG: u32 = 0x8000_0000;

/// 页框号转换为物理地址
#[inline]
pub fn pfn_to_phys(pfn: usize) -> usize {
    pfn << 12
}

/// 物理地址转换为页框号
#[inline]
pub fn phys_to_pfn(phys: usize) -> usize {
    phys >> 12
}

/// 初始化页面的引用计数（用于新分配的页面）
pub fn page_ref_init(pfn: usize) {
    page_ref_set(pfn, 1);
}

/// 获取页面并增加引用
pub fn get_page(pfn: usize) -> bool {
    match slot(pfn) {
        Some(s) => (s.fetch_add(1, Ordering::Relaxed) & 0xFFFF) != 0,
        None => false,
    }
}

/// 释放页面并减少引用
/// 返回 true 如果页面应该被释放（引用计数降到 0）
pub fn put_page(pfn: usize) -> bool {
    match slot(pfn) {
        Some(s) => (s.fetch_sub(1, Ordering::Relaxed) & 0xFFFF) <= 1,
        None => false,
    }
}

/// 检查页面是否只被一个进程使用
pub fn page_is_unique(pfn: usize) -> bool {
    page_ref_count(pfn) == 1
}

/// 尝试复制页面（COW）
/// 如果页面只被一个进程使用，直接设置写权限
/// 如果页面被多个进程共享，分配新页面并复制内容
/// 
/// 返回新页面的物理地址，0 表示失败
pub fn cow_copy_page(old_pfn: usize, old_flags: u64) -> usize {
    let ref_count = page_ref_count(old_pfn);
    
    if ref_count == 1 {
        // 只有一个引用，可以直接写
        return old_pfn;
    }
    
    // 需要复制页面
    let new_pfn = alloc_page_for_cow();
    if new_pfn == 0 {
        return 0;
    }
    
    // 复制内容
    let old_virt = pfn_to_phys(old_pfn);
    let new_virt = pfn_to_phys(new_pfn);
    
    unsafe {
        let src = old_virt as *const u8;
        let dst = new_virt as *mut u8;
        core::ptr::copy_nonoverlapping(src, dst, 4096);
    }
    
    // 减少原页面的引用计数
    page_ref_dec(old_pfn);
    
    // 设置新页面的引用计数为 1
    page_ref_set(new_pfn, 1);
    page_set_cow(new_pfn, false);
    
    new_pfn
}

/// 为 COW 分配新页面
fn alloc_page_for_cow() -> usize {
    // 使用现有的页面分配器
    use crate::mm::page_alloc::get_free_page;
    let phys = get_free_page();
    if phys == 0 {
        return 0;
    }
    let pfn = phys_to_pfn(phys);
    page_ref_init(pfn);
    pfn
}

/// COW 页面的页面错误处理
/// 
/// # Arguments
/// * `fault_addr` - 触发 page fault 的虚拟地址
/// * `pml4` - 当前进程的页表
/// 
/// # Returns
/// * `true` - 页面已正确映射
/// * `false` - 处理失败
pub fn handle_cow_fault(fault_addr: usize, pml4: usize) -> bool {
    use crate::mm::paging;
    
    // 首先检查页表项是否存在
    let pte = unsafe { paging::translate(pml4, fault_addr) };
    if pte.is_none() {
        return false;
    }
    
    let phys = pte.unwrap();
    let pfn = phys_to_pfn(phys);
    
    // 检查是否是 COW 页面
    if !page_is_cow(pfn) {
        return true; // 不是 COW 页面，直接返回
    }
    
    // 获取当前页面的权限标志
    // 这里需要读取页表项
    // 如果需要写且是 COW，执行复制
    
    let new_pfn = cow_copy_page(pfn, 0);
    if new_pfn == 0 {
        return false;
    }
    
    // 取消映射旧页面并建立新映射
    unsafe {
        let _ = paging::unmap_page(pml4, fault_addr);
        
        // 使用读写权限重新映射
        let new_phys = pfn_to_phys(new_pfn);
        return paging::map_page(pml4, fault_addr, new_phys, paging::flags::SHARED);
    }
}

/// 标记页面为 COW
pub fn mark_page_cow(pfn: usize) {
    if page_ref_count(pfn) > 1 {
        page_set_cow(pfn, true);
    }
}

/// 取消页面的 COW 标记（当所有共享者都分离后）
pub fn unmark_page_cow(pfn: usize) {
    page_set_cow(pfn, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_ref_init() {
        init();
        page_ref_init(100);
        assert_eq!(page_ref_count(100), 1);
    }

    #[test]
    fn test_page_ref_inc_dec() {
        init();
        page_ref_init(101);
        assert_eq!(page_ref_count(101), 1);
        
        page_ref_inc(101);
        assert_eq!(page_ref_count(101), 2);
        
        page_ref_dec(101);
        assert_eq!(page_ref_count(101), 1);
    }

    #[test]
    fn test_cow_flag() {
        init();
        page_ref_init(102);
        
        assert!(!page_is_cow(102));
        page_set_cow(102, true);
        assert!(page_is_cow(102));
        page_set_cow(102, false);
        assert!(!page_is_cow(102));
    }
}
