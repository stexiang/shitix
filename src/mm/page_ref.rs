//! 页面引用计数管理
//!
//! 用于实现 COW (Copy-On-Write) 机制。当多个进程共享同一物理页时，
//! 使用引用计数跟踪共享情况，只有当引用计数降到 1 时才能进行写时复制。

use core::sync::atomic::{AtomicU32, Ordering};

/// 页面引用计数数组 - 每个物理页框一个计数
/// 假设最大 256MB 物理内存，页大小 4KB，则最多 65536 页
const MAX_PFN: usize = 64 * 1024;

/// 页面引用计数表
/// 使用静态数组存储，每个物理页框对应一个引用计数
static mut PAGE_REF_ARRAY: [AtomicU32; 65536] = [const { AtomicU32::new(0) }; 65536];

/// 初始化页面引用计数
pub fn init() {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(PAGE_REF_ARRAY);
        let len = (*ptr).len();
        for i in 0..len {
            (*ptr)[i].store(0, Ordering::Relaxed);
        }
        crate::pr_info!("page_ref: initialized {} slots", len);
    }
}

/// 获取页框的引用计数
#[inline]
pub fn page_ref_count(pfn: usize) -> u32 {
    if pfn >= 65536 {
        return 0;
    }
    unsafe {
        PAGE_REF_ARRAY[pfn].load(Ordering::Relaxed) & 0xFFFF
    }
}

/// 增加页框引用计数
/// 返回新的引用计数
#[inline]
pub fn page_ref_inc(pfn: usize) -> u32 {
    if pfn >= 65536 {
        return 0;
    }
    unsafe {
        let old = PAGE_REF_ARRAY[pfn].fetch_add(1, Ordering::Relaxed);
        (old + 1) & 0xFFFF
    }
}

/// 减少页框引用计数
/// 返回新的引用计数
#[inline]
pub fn page_ref_dec(pfn: usize) -> u32 {
    if pfn >= 65536 {
        return 0;
    }
    unsafe {
        let old = PAGE_REF_ARRAY[pfn].fetch_sub(1, Ordering::Relaxed);
        (old - 1) & 0xFFFF
    }
}

/// 设置页框引用计数
#[inline]
pub fn page_ref_set(pfn: usize, count: u32) {
    if pfn >= 65536 {
        return;
    }
    unsafe {
        PAGE_REF_ARRAY[pfn].store(count & 0xFFFF, Ordering::Relaxed);
    }
}

/// 获取页框的 COW 标记
#[inline]
pub fn page_is_cow(pfn: usize) -> bool {
    if pfn >= 65536 {
        return false;
    }
    unsafe {
        (PAGE_REF_ARRAY[pfn].load(Ordering::Relaxed) & 0x8000) != 0
    }
}

/// 设置页框的 COW 标记
#[inline]
pub fn page_set_cow(pfn: usize, cow: bool) {
    if pfn >= 65536 {
        return;
    }
    unsafe {
        let val = PAGE_REF_ARRAY[pfn].load(Ordering::Relaxed);
        if cow {
            PAGE_REF_ARRAY[pfn].store(val | 0x8000, Ordering::Relaxed);
        } else {
            PAGE_REF_ARRAY[pfn].store(val & !0x8000, Ordering::Relaxed);
        }
    }
}

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
    if pfn >= 65536 {
        return false;
    }
    unsafe {
        let old = PAGE_REF_ARRAY[pfn].fetch_add(1, Ordering::Relaxed);
        (old & 0xFFFF) != 0
    }
}

/// 释放页面并减少引用
/// 返回 true 如果页面应该被释放（引用计数降到 0）
pub fn put_page(pfn: usize) -> bool {
    if pfn >= 65536 {
        return false;
    }
    unsafe {
        let old = PAGE_REF_ARRAY[pfn].fetch_sub(1, Ordering::Relaxed);
        (old & 0xFFFF) <= 1
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
