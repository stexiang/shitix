//! 页表操作：建立映射、查询、撤销。
//!
//! 对应 linux-1.0.9 的 `mm/memory.c` 中的 `remap_page_range()` /
//! `put_page()` / `unmap_page_range()` / `invalidate()`，以及
//! `include/linux/head.h` 里的 `PAGE_PRESENT` 等页保护位。
//!
//! 与原版最大的差异是级数：原版 32 位是「页目录 → 页表」两级，
//! 这里 x86_64 是「PML4 → PDPT → PD → PT」四级。原版 `PAGE_DIR_OFFSET` /
//! `PAGE_PTR` 两个宏在这里变成 [`pml4_index`] 等四个取下标函数。
//!
//! setup.S 已经用 2MB 大页恒等映射了低 1GB；本模块用来在此之上建立
//! 4KB 粒度的新映射（后续 vmalloc / 用户空间需要）。

use super::page::{PAGE_SIZE, PTRS_PER_PAGE, page_base};
use super::page_alloc::get_free_page;

/// 页表项标志位。对应原版 `head.h` 的 `PAGE_PRESENT`/`PAGE_RW`/`PAGE_USER` 等。
pub mod flags {
    pub const PRESENT: u64 = 1 << 0;
    pub const RW: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    pub const ACCESSED: u64 = 1 << 5;
    pub const DIRTY: u64 = 1 << 6;
    /// 2MB/1GB 大页标记（原版 32 位内核未用到，setup.S 建映射时用了）
    pub const HUGE: u64 = 1 << 7;
    pub const NO_EXEC: u64 = 1 << 63;

    /// 同原版 `PAGE_SHARED`：present + rw + user
    pub const SHARED: u64 = PRESENT | RW | USER;
    /// 同原版 `PAGE_COPY`/`PAGE_READONLY`：present + user，不可写
    pub const READONLY: u64 = PRESENT | USER;
    /// 内核页：present + rw，不给用户态
    pub const KERNEL: u64 = PRESENT | RW;
}

/// 抹掉标志位、取出物理地址的掩码（52 位物理地址空间）。
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

#[inline]
fn pml4_index(v: usize) -> usize {
    (v >> 39) & 0x1ff
}
#[inline]
fn pdpt_index(v: usize) -> usize {
    (v >> 30) & 0x1ff
}
#[inline]
fn pd_index(v: usize) -> usize {
    (v >> 21) & 0x1ff
}
#[inline]
fn pt_index(v: usize) -> usize {
    (v >> 12) & 0x1ff
}

/// 刷 TLB。对应原版 `invalidate()`（原版是 `movl %%cr3,%%eax; movl %%eax,%%cr3`）。
pub fn invalidate() {
    // SAFETY: 读写 cr3 在 CPL=0 合法；写回原值只会刷新 TLB，不改变映射内容。
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        // 写 cr3 换页表 = 改变之后所有内存访问的含义。不能声明
        // `nostack`（栈也是内存，映射可能变），也不能让编译器把跨过这条
        // 指令的访问重排。只留 preserves_flags。
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(preserves_flags));
    }
}

/// 刷单个页的 TLB（比整表刷新便宜，原版没有，因为 386 没有 invlpg）。
pub fn invalidate_page(vaddr: usize) {
    // SAFETY: invlpg 在 CPL=0 合法，只影响 TLB 缓存，不改映射。
    unsafe {
        // 同写 cr3：invlpg 改变该页后续访问的解析结果，不能声明 nostack。
        core::arch::asm!("invlpg [{}]", in(reg) vaddr, options(preserves_flags));
    }
}

/// 取当前 cr3 指向的 PML4 物理地址。
pub fn current_pml4() -> usize {
    // SAFETY: 读 cr3 在 CPL=0 合法。
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) }
    (cr3 & ADDR_MASK) as usize
}

/// 读一个表项。
///
/// # Safety
/// `table` 必须是页对齐的、位于恒等映射范围内的页表物理地址，`idx < 512`。
unsafe fn entry(table: usize, idx: usize) -> u64 {
    debug_assert!(idx < PTRS_PER_PAGE);
    // SAFETY: 由调用者契约保证 table 是有效页表页且被恒等映射；
    // idx < 512 使偏移落在这一页内。用 volatile 防止编译器缓存页表内容。
    unsafe { core::ptr::read_volatile((table as *const u64).add(idx)) }
}

/// 写一个表项。
///
/// # Safety
/// 同 [`entry`]。调用者还要负责在必要时刷 TLB。
unsafe fn set_entry(table: usize, idx: usize, val: u64) {
    debug_assert!(idx < PTRS_PER_PAGE);
    // SAFETY: 同 entry 的契约；写入后由调用者刷 TLB 保证一致性。
    unsafe { core::ptr::write_volatile((table as *mut u64).add(idx), val) }
}

/// 沿着某一级往下走一步：若该项不存在则分配一张新页表页。
/// 对应原版 `put_page()` 里那段「页目录项为空就 get_free_page 建页表」。
///
/// # Safety
/// `table` 必须是有效的、被恒等映射的页表页物理地址。
unsafe fn next_level(table: usize, idx: usize, user: bool) -> Option<usize> {
    // SAFETY: 由调用者契约保证 table 有效。
    unsafe {
        let e = entry(table, idx);
        if e & flags::PRESENT != 0 {
            if e & flags::HUGE != 0 {
                // 撞到 setup.S 建的 2MB 大页：不在这里做拆分，交给调用者处理
                return None;
            }
            return Some((e & ADDR_MASK) as usize);
        }
        let page = get_free_page();
        if page == 0 {
            return None;
        }
        let mut f = flags::PRESENT | flags::RW;
        if user {
            f |= flags::USER;
        }
        set_entry(table, idx, page as u64 | f);
        Some(page)
    }
}

/// 把虚拟地址 `vaddr` 映射到物理地址 `paddr`，权限由 `prot` 指定。
/// 对应原版 `put_page(struct task_struct*, unsigned long page, unsigned long address)`。
///
/// 返回 `false` 表示中途没内存了，或路径上撞到大页无法细化。
///
/// # Safety
/// `pml4` 必须是有效的四级页表根物理地址。调用者要保证这次映射不会
/// 破坏正在使用的内存（例如覆盖内核自身的映射）。
pub unsafe fn map_page(pml4: usize, vaddr: usize, paddr: usize, prot: u64) -> bool {
    let user = prot & flags::USER != 0;
    // SAFETY: 由调用者契约保证 pml4 有效；下面每一级都用 next_level 校验 PRESENT。
    unsafe {
        let Some(pdpt) = next_level(pml4, pml4_index(vaddr), user) else { return false };
        let Some(pd) = next_level(pdpt, pdpt_index(vaddr), user) else { return false };
        let Some(pt) = next_level(pd, pd_index(vaddr), user) else { return false };
        set_entry(pt, pt_index(vaddr), (page_base(paddr) as u64) | prot | flags::PRESENT);
    }
    invalidate_page(vaddr);
    true
}

/// 映射一段连续物理内存。对应原版 `remap_page_range()`。
///
/// # Safety
/// 同 [`map_page`]；`size` 会被向上取整到整页。
pub unsafe fn map_range(pml4: usize, vaddr: usize, paddr: usize, size: usize, prot: u64) -> bool {
    let pages = super::page::page_align(size) / PAGE_SIZE;
    for i in 0..pages {
        // SAFETY: 逐页调用 map_page，契约由本函数的调用者继承。
        unsafe {
            if !map_page(pml4, vaddr + i * PAGE_SIZE, paddr + i * PAGE_SIZE, prot) {
                return false;
            }
        }
    }
    true
}

/// 查询虚拟地址当前映射到的物理地址，未映射返回 `None`。
/// 用于自检与将来的 page fault 处理。
///
/// # Safety
/// `pml4` 必须是有效的四级页表根物理地址。
pub unsafe fn translate(pml4: usize, vaddr: usize) -> Option<usize> {
    // SAFETY: 每级都先查 PRESENT 再往下走，不会解引用无效页表。
    unsafe {
        let e = entry(pml4, pml4_index(vaddr));
        if e & flags::PRESENT == 0 {
            return None;
        }
        let pdpt = (e & ADDR_MASK) as usize;

        let e = entry(pdpt, pdpt_index(vaddr));
        if e & flags::PRESENT == 0 {
            return None;
        }
        if e & flags::HUGE != 0 {
            // 1GB 大页
            return Some(((e & ADDR_MASK) as usize) | (vaddr & ((1 << 30) - 1)));
        }
        let pd = (e & ADDR_MASK) as usize;

        let e = entry(pd, pd_index(vaddr));
        if e & flags::PRESENT == 0 {
            return None;
        }
        if e & flags::HUGE != 0 {
            // 2MB 大页（setup.S 给低 1GB 建的就是这种）
            return Some(((e & ADDR_MASK) as usize) | (vaddr & ((1 << 21) - 1)));
        }
        let pt = (e & ADDR_MASK) as usize;

        let e = entry(pt, pt_index(vaddr));
        if e & flags::PRESENT == 0 {
            return None;
        }
        Some(((e & ADDR_MASK) as usize) | (vaddr & (PAGE_SIZE - 1)))
    }
}

/// 撤销一页映射，返回它原先指向的物理地址。
/// 对应原版 `unmap_page_range()`（原版会顺带 `free_page`，这里把释放交给调用者）。
///
/// # Safety
/// `pml4` 必须有效；调用者要确保没人还在用这段虚拟地址。
pub unsafe fn unmap_page(pml4: usize, vaddr: usize) -> Option<usize> {
    // SAFETY: 逐级检查 PRESENT，只在四级页表齐备时才改最后一级。
    unsafe {
        let e = entry(pml4, pml4_index(vaddr));
        if e & flags::PRESENT == 0 {
            return None;
        }
        let pdpt = (e & ADDR_MASK) as usize;
        let e = entry(pdpt, pdpt_index(vaddr));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 {
            return None;
        }
        let pd = (e & ADDR_MASK) as usize;
        let e = entry(pd, pd_index(vaddr));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 {
            return None;
        }
        let pt = (e & ADDR_MASK) as usize;
        let e = entry(pt, pt_index(vaddr));
        if e & flags::PRESENT == 0 {
            return None;
        }
        set_entry(pt, pt_index(vaddr), 0);
        invalidate_page(vaddr);
        Some((e & ADDR_MASK) as usize)
    }
}

/// 复制页表项到新的页表
/// 
/// 复制源页表的映射到目标页表，适用于 fork 时的页表复制。
/// 
/// # Safety
/// - src_pml4 和 dst_pml4 必须是有效的四级页表根
/// - 此函数不处理 COW 标记，需要调用者设置
pub unsafe fn copy_page_table(src_pml4: usize, dst_pml4: usize, user: bool) -> bool {
    // 用户空间范围: 0x40000000 - 0xFFFF_FFFF
    const USERSPACE_START: usize = 0x4000_0000;
    const USERSPACE_END: usize = 0xFFFF_FFFF;
    
    // 遍历用户空间的每一页
    let mut vaddr = USERSPACE_START;
    while vaddr < USERSPACE_END {
        // 检查源页表中的映射
        if let Some(phys) = unsafe { translate(src_pml4, vaddr) } {
            // 获取当前权限
            let prot = unsafe {
                let pte = entry(pml4_index(vaddr), pml4_index(vaddr));
                let pdpt = (pte & ADDR_MASK) as usize;
                let pde = entry(pdpt, pdpt_index(vaddr));
                let pt_base = (pde & ADDR_MASK) as usize;
                let pte_val = entry(pt_base, pt_index(vaddr));
                pte_val & 0xFFF  // 获取标志位
            };
            
            // 在目标页表中创建映射
            if !map_page(dst_pml4, vaddr, phys, prot | flags::PRESENT | flags::USER) {
                return false;
            }
        }
        vaddr += PAGE_SIZE;
    }
    true
}

/// 复制并设置 COW 页表
/// 
/// 复制父进程的页表到子进程，将所有页面设置为只读 (COW)。
/// 
/// # Safety
/// 同 copy_page_table
pub unsafe fn cow_copy_page_table(src_pml4: usize, dst_pml4: usize) -> bool {
    // 用户空间范围
    const USERSPACE_START: usize = 0x4000_0000;
    const USERSPACE_END: usize = 0xFFFF_FFFF;
    
    let mut vaddr = USERSPACE_START;
    while vaddr < USERSPACE_END {
        // 检查源页表中的映射
        if let Some(phys) = unsafe { translate(src_pml4, vaddr) } {
            // 设置为只读 (COW)
            let cow_prot = flags::PRESENT | flags::USER;  // 没有 RW 标志
            
            if !map_page(dst_pml4, vaddr, phys, cow_prot) {
                return false;
            }
        }
        vaddr += PAGE_SIZE;
    }
    true
}

/// 获取页表项的权限标志
pub fn get_page_flags(pml4: usize, vaddr: usize) -> Option<u64> {
    unsafe {
        let e = entry(pml4, pml4_index(vaddr));
        if e & flags::PRESENT == 0 {
            return None;
        }
        let pdpt = (e & ADDR_MASK) as usize;
        
        let e = entry(pdpt, pdpt_index(vaddr));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 {
            return None;
        }
        let pd = (e & ADDR_MASK) as usize;
        
        let e = entry(pd, pd_index(vaddr));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 {
            return None;
        }
        let pt = (e & ADDR_MASK) as usize;
        
        let e = entry(pt, pt_index(vaddr));
        if e & flags::PRESENT == 0 {
            return None;
        }
        
        Some(e & 0xFFF)
    }
}

/// 设置页表项的权限
pub fn set_page_flags(pml4: usize, vaddr: usize, new_flags: u64) -> bool {
    unsafe {
        let e = entry(pml4, pml4_index(vaddr));
        if e & flags::PRESENT == 0 {
            return false;
        }
        let pdpt = (e & ADDR_MASK) as usize;
        
        let e = entry(pdpt, pdpt_index(vaddr));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 {
            return false;
        }
        let pd = (e & ADDR_MASK) as usize;
        
        let e = entry(pd, pd_index(vaddr));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 {
            return false;
        }
        let pt = (e & ADDR_MASK) as usize;
        
        let e = entry(pt, pt_index(vaddr));
        if e & flags::PRESENT == 0 {
            return false;
        }
        
        let phys = e & ADDR_MASK;
        set_entry(pt, pt_index(vaddr), phys | new_flags | flags::PRESENT);
        invalidate_page(vaddr);
        true
    }
}
