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
    /// 页级写穿（bit 3）。MMIO 映射用。
    pub const PWT: u64 = 1 << 3;
    /// 页级禁用缓存（bit 4）。MMIO（如 LAPIC 0xFEE00000）必须禁缓存。
    pub const PCD: u64 = 1 << 4;
    pub const ACCESSED: u64 = 1 << 5;
    pub const DIRTY: u64 = 1 << 6;
    /// 2MB/1GB 大页标记（原版 32 位内核未用到，setup.S 建映射时用了）
    pub const HUGE: u64 = 1 << 7;
    /// 延迟分配标记：叶子 PTE 里 PRESENT=0 但置此位，表示「虚拟地址已保留、
    /// 物理页待首次访问时再分配」（对应 Linux mmap/brk 的惰性语义）。
    /// 用 bit 9（x86_64 软件可用位），不与 PRESENT/RW/USER 冲突。
    pub const RESERVED: u64 = 1 << 9;
    /// 换出到 swap 的页（PRESENT=0 + SWAPPED，bits 12+ 存槽号）。
    pub const SWAPPED: u64 = 1 << 10;
    pub const NO_EXEC: u64 = 1 << 63;

    /// 同原版 `PAGE_SHARED`：present + rw + user
    pub const SHARED: u64 = PRESENT | RW | USER;
    /// 同原版 `PAGE_COPY`/`PAGE_READONLY`：present + user，不可写
    pub const READONLY: u64 = PRESENT | USER;
    /// 内核页：present + rw，不给用户态
    pub const KERNEL: u64 = PRESENT | RW;
}

/// 抹掉标志位、取出物理地址的掩码（52 位物理地址空间）。
pub const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

/// 高半区直接映射基址：物理 0..1GB 映射到 `PHYS_MAP_BASE .. PHYS_MAP_BASE+1GB`。
///
/// 低 1GB 的恒等映射（setup.S 建）被用户 ELF（装在 0x400000 起）覆盖后，
/// 内核若仍按「物理==虚拟」访问物理内存（空闲链表、页表、缓冲缓存、用户页），
/// 读到的就是用户 ELF 的页而非真实物理页 → get_free_page_raw 读空闲链表
/// 节点时对 clobber 掉的物理地址死循环缺页（g++/cc1plus 卡死根因）。
/// 高半区直接映射用户态映射不到，内核据此访问物理内存，与用户地址空间彻底分离。
pub const PHYS_MAP_BASE: usize = 0xffff_8000_0000_0000;

#[inline]
pub fn pml4_index(v: usize) -> usize {
    (v >> 39) & 0x1ff
}
#[inline]
pub fn pdpt_index(v: usize) -> usize {
    (v >> 30) & 0x1ff
}
#[inline]
pub fn pd_index(v: usize) -> usize {
    (v >> 21) & 0x1ff
}
#[inline]
pub fn pt_index(v: usize) -> usize {
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
/// `table` 必须是页对齐的页表物理地址，`idx < 512`。经 [`PHYS_MAP_BASE`]
/// 高半区直接映射访问，避免低 1GB 恒等映射被用户 ELF 覆盖后读错。
pub unsafe fn entry(table: usize, idx: usize) -> u64 {
    debug_assert!(idx < PTRS_PER_PAGE);
    // SAFETY: 由调用者契约保证 table 是有效页表页；PHYS_MAP_BASE 覆盖物理
    // 0..1GB，table 落在其中即被映射。volatile 防止编译器缓存页表内容。
    unsafe { core::ptr::read_volatile(((PHYS_MAP_BASE + table) as *const u64).add(idx)) }
}

/// 写一个表项。
///
/// # Safety
/// 同 [`entry`]。调用者还要负责在必要时刷 TLB。
unsafe fn set_entry(table: usize, idx: usize, val: u64) {
    debug_assert!(idx < PTRS_PER_PAGE);
    // SAFETY: 同 entry 的契约；写入后由调用者刷 TLB 保证一致性。
    unsafe { core::ptr::write_volatile(((PHYS_MAP_BASE + table) as *mut u64).add(idx), val) }
}

/// 沿着某一级往下走一步：若该项不存在则分配一张新页表页。
/// 对应原版 `put_page()` 里那段「页目录项为空就 get_free_page 建页表」。
///
/// # Safety
/// `table` 必须是有效的、被恒等映射的页表页物理地址。
unsafe fn next_level(table: usize, idx: usize, user: bool) -> Option<usize> {
    unsafe {
        let e = entry(table, idx);
        if e & flags::PRESENT != 0 {
            if e & flags::HUGE != 0 {
                // Split 2MB large page for user-space access.
                // Strip GLOBAL flag — TLB entries with GLOBAL persist across CR3
                // switches and would point to old 2MB pages instead of new 4KB ones.
                let pt = get_free_page();
                if pt == 0 { return None; }
                let phys = e & ADDR_MASK;
                // Keep PRESENT|RW but NEVER add USER: the split entries cover
                // the entire 2MB region including free pages that contain
                // allocator metadata (free-list pointers). USER flag is added
                // later by map_page only for the specific pages the ELF loader
                // allocates. Also strip HUGE and GLOBAL.
                let base_flags = (e & !ADDR_MASK & !flags::HUGE & !0x100) | flags::PRESENT;
                for i in 0..512 {
                    set_entry(pt, i, (phys + i as u64 * 4096) | base_flags);
                }
                let mut new_e = (pt as u64) | base_flags;
                if user { new_e |= flags::USER; }
                set_entry(table, idx, new_e);
                return Some(pt);
            }
            // 条目已存在：追加 USER 位（如果需要且尚未设置）。
            if user && (e & flags::USER == 0) {
                set_entry(table, idx, e | flags::USER);
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

/// 读叶子页表项原值（即使 PRESENT=0，如 RESERVED/SWAPPED 项）。
/// 中间层缺页或遇巨页时返回 None。
///
/// # Safety
/// 同 [`translate`]。
pub unsafe fn leaf_entry(pml4: usize, vaddr: usize) -> Option<u64> {
    // SAFETY: 逐级查 PRESENT 再下钻（SWAPPED/RESERVED 叶子只在 PRESENT 的
    // 中间层之下出现）。
    unsafe {
        let e = entry(pml4, pml4_index(vaddr));
        if e & flags::PRESENT == 0 { return None; }
        let pdpt = (e & ADDR_MASK) as usize;
        let e = entry(pdpt, pdpt_index(vaddr));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 { return None; }
        let pd = (e & ADDR_MASK) as usize;
        let e = entry(pd, pd_index(vaddr));
        if e & flags::PRESENT == 0 || e & flags::HUGE != 0 { return None; }
        let pt = (e & ADDR_MASK) as usize;
        Some(entry(pt, pt_index(vaddr)))
    }
}

/// 直接写叶子页表项原值（可以是 PRESENT=0 的 SWAPPED/RESERVED 项）。
/// 中间表按需创建；普通映射请走 [`map_page`]。
///
/// # Safety
/// 同 [`map_page`]。
pub unsafe fn set_leaf_entry(pml4: usize, vaddr: usize, val: u64) -> bool {
    let user = val & flags::USER != 0;
    // SAFETY: 契约同 map_page；中间级由 next_level 建表。
    unsafe {
        let Some(pdpt) = next_level(pml4, pml4_index(vaddr), user) else { return false };
        let Some(pd) = next_level(pdpt, pdpt_index(vaddr), user) else { return false };
        let Some(pt) = next_level(pd, pd_index(vaddr), user) else { return false };
        set_entry(pt, pt_index(vaddr), val);
    }
    invalidate_page(vaddr);
    true
}

/// 保留一段虚拟地址空间，但不分配物理页（惰性分配）。
///
/// 对应 Linux `mmap`/`brk` 的语义：调用只记账，物理页等真正访问时由
/// [`resolve_reserved`]（page fault handler 里）再分配。glibc 的 malloc
/// 会在初始化时 `mmap(NULL, ~1.4GB, RW, ANON)` 预留一大段主竞技场，Linux
/// 下这只占虚拟空间；若内核这里按旧实现每页都 `get_free_page` 一张物理页，
/// 256MB 内存直接被这段预留吃光，后续 fork 全部 EAGAIN。
///
/// # Safety
/// 同 [`map_page`]。
pub unsafe fn map_reserved(pml4: usize, vaddr: usize, prot: u64) -> bool {
    let user = prot & flags::USER != 0;
    // SAFETY: 同 map_page；中间级由 next_level 建表。
    unsafe {
        let Some(pdpt) = next_level(pml4, pml4_index(vaddr), user) else { return false };
        let Some(pd) = next_level(pdpt, pdpt_index(vaddr), user) else { return false };
        let Some(pt) = next_level(pd, pd_index(vaddr), user) else { return false };
        // 叶子 PTE：PRESENT=0 + RESERVED + 保留 RW/USER/NO_EXEC，物理地址 0。
        let leaf = flags::RESERVED | (prot & (flags::RW | flags::USER | flags::NO_EXEC));
        set_entry(pt, pt_index(vaddr), leaf);
    }
    true
}

/// 查询虚拟地址是否处于「已保留但未分配」状态。
///
/// # Safety
/// 同 [`translate`]。
pub unsafe fn is_reserved(pml4: usize, vaddr: usize) -> bool {
    // SAFETY: 逐级查 PRESENT 再下钻。
    unsafe {
        let e = entry(pml4, pml4_index(vaddr));
        if e & flags::PRESENT == 0 { return false; }
        let pdpt = (e & ADDR_MASK) as usize;
        let e = entry(pdpt, pdpt_index(vaddr));
        if e & flags::PRESENT == 0 { return false; }
        let pd = (e & ADDR_MASK) as usize;
        let e = entry(pd, pd_index(vaddr));
        if e & flags::PRESENT == 0 { return false; }
        let pt = (e & ADDR_MASK) as usize;
        let e = entry(pt, pt_index(vaddr));
        e & flags::RESERVED != 0
    }
}

/// 把一个「已保留」的虚拟页落实为物理页。
///
/// 供 page fault handler 在首次访问惰性分配页时调用。返回 true 表示已分配
/// 并映射好；false 表示该地址不是保留页或内存耗尽。
///
/// # Safety
/// 同 [`map_page`]；必须只在「该页确实处于 reserved 状态」时调用。
pub unsafe fn resolve_reserved(pml4: usize, vaddr: usize) -> bool {
    // SAFETY: 逐级查 PRESENT。
    unsafe {
        let Some(pdpt) = next_level(pml4, pml4_index(vaddr), true) else { return false };
        let Some(pd) = next_level(pdpt, pdpt_index(vaddr), true) else { return false };
        let Some(pt) = next_level(pd, pd_index(vaddr), true) else { return false };
        let idx = pt_index(vaddr);
        let leaf = entry(pt, idx);
        if leaf & flags::RESERVED == 0 {
            return false; // 不是保留页
        }
        let pg = get_free_page();
        if pg == 0 {
            return false;
        }
        // 保留时的 RW/USER/NO_EXEC 原样带上，再加 PRESENT（物理地址 = pg）。
        let prot = leaf & (flags::RW | flags::USER | flags::NO_EXEC);
        set_entry(pt, idx, (pg as u64) | prot | flags::PRESENT);
        invalidate_page(vaddr);
    }
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

/// 查询虚拟地址是否映射为**用户可访问**的页（最终 PTE 含 PRESENT|USER）。
///
/// 与 [`translate`] 的区别：后者只看 PRESENT，会把「拆分 2MB 内核大页后留下的
/// present-but-not-user 的 4KB 表项」也算作已映射。brk 等用户内存分配器必须用本函数，
/// 否则会把内核拆分页当成「已分配的用户页」而跳过映射，导致用户态访问这些页时
/// 触发 err=0x5 的保护故障（present + read + user）。
///
/// # Safety
/// `pml4` 必须是有效的四级页表根物理地址。
pub unsafe fn is_user_mapped(pml4: usize, vaddr: usize) -> bool {
    // SAFETY: 每级都先查 PRESENT 再往下走，不会解引用无效页表。
    unsafe {
        let e = entry(pml4, pml4_index(vaddr));
        if e & flags::PRESENT == 0 {
            return false;
        }
        let pdpt = (e & ADDR_MASK) as usize;

        let e = entry(pdpt, pdpt_index(vaddr));
        if e & flags::PRESENT == 0 {
            return false;
        }
        if e & flags::HUGE != 0 {
            return e & flags::USER != 0;
        }
        let pd = (e & ADDR_MASK) as usize;

        let e = entry(pd, pd_index(vaddr));
        if e & flags::PRESENT == 0 {
            return false;
        }
        if e & flags::HUGE != 0 {
            return e & flags::USER != 0;
        }
        let pt = (e & ADDR_MASK) as usize;

        let e = entry(pt, pt_index(vaddr));
        e & (flags::PRESENT | flags::USER) == (flags::PRESENT | flags::USER)
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
            if e & flags::RESERVED != 0 {
                // RESERVED（惰性分配）叶子：munmap/错误回滚也要摘掉，
                // 否则区间内残留的保留项会让之后的访问误解析成匿名页。
                // 无物理页可还，返回 None。
                set_entry(pt, pt_index(vaddr), 0);
                invalidate_page(vaddr);
            }
            return None;
        }
        set_entry(pt, pt_index(vaddr), 0);
        invalidate_page(vaddr);
        Some((e & ADDR_MASK) as usize)
    }
}

/// 分配一个空的 PML4 页，清零并返回物理地址。
/// 调用者负责把 PML4[0] 填上内核页表条目再使用。
pub fn alloc_pml4() -> usize {
    let p = get_free_page();
    if p == 0 {
        return 0;
    }
    // SAFETY：刚分配的页，物理地址有效且在恒等映射内。
    unsafe {
        core::ptr::write_bytes(p as *mut u8, 0, PAGE_SIZE);
    }
    p
}

/// 在新分配的用户 PML4 里建立内核映射。
///
/// 分配独立的 PDPT + PD 页，复制内核 PD 条目。
/// 这样用户进程的 2MB 页拆分不会污染共享内核 PD(0x6000)。
///
/// 返回 false 表示 `dst_pml4 == 0` 或内存不足。
pub fn clone_kernel_pdpt(dst_pml4: usize) -> bool {
    if dst_pml4 == 0 {
        return false;
    }
    let pdpt = get_free_page();
    let pd = get_free_page();
    if pdpt == 0 || pd == 0 {
        if pdpt != 0 { crate::mm::free_page(pdpt); }
        if pd != 0 { crate::mm::free_page(pd); }
        return false;
    }
    // SAFETY：dst_pml4 / pdpt / pd 经高半区直接映射可写（物理 0..1GB 都覆盖）。
    unsafe {
        // Copy kernel PD entries (512 × 8 bytes = 4096 bytes) from boot PD 0x6000
        // 经高半区直接映射，避免恒等映射被用户 ELF 覆盖后读错。
        core::ptr::copy_nonoverlapping(
            (PHYS_MAP_BASE + 0x6000) as *const u8,
            (PHYS_MAP_BASE + pd) as *mut u8,
            4096,
        );
        // PML4[0] → new PDPT
        set_entry(dst_pml4, 0, pdpt as u64 | flags::PRESENT | flags::RW);
        // PDPT[0] → NEW PD (not shared 0x6000)
        set_entry(pdpt, 0, pd as u64 | flags::PRESENT | flags::RW);
        // 共享 boot PDPT(0x5000) 的第 3 项（3-4GB）：smp_init 的 LAPIC、
        // e1000 的 MMIO、fb 的 LFB 都映在这棵子树里，且只进了内核引导
        // PML4。不共享的话，内核在用户 CR3 下一碰 LAPIC（this_cpu 读
        // APIC ID）就缺页，缺页处理再读 LAPIC → 递归 #PF 风暴挂死。
        // 共享 entry（而非复制子树）还能让事后新增的 MMIO 映射
        // （如 fb auto_init）自动对所有已建用户 PML4 可见。
        // 中间级不带 USER 位，ring-3 够不到 MMIO。
        let mmio = entry(0x5000, 3);
        if mmio != 0 {
            set_entry(pdpt, 3, mmio);
        }
        // 复制高半区直接映射（boot PML4[256] → 高 PDPT → boot PD 0x6000），
        // 让每个用户 PML4 都能在用户 ELF 覆盖低 1GB 后仍访问物理内存。
        let high = entry(0x4000, 256);
        set_entry(dst_pml4, 256, high);
    }
    true
}

/// 建立高半区直接映射：物理 0..1GB → [`PHYS_MAP_BASE`]..+1GB。
///
/// 用一个新 PDPT，其 [0] 指向 boot PD(0x6000)（setup.S 建的恒等映射 PD，
/// 512 个 2MB 大页）。这样 `PHYS_MAP_BASE + phys` 恒等于 phys，用户态映射
/// （低 0x400000 起）永远覆盖不到这一半，内核据此访问物理内存。
///
/// 必须在用户 ELF 装载前调用（此时恒等映射完好，可安全直写高 PDPT 与 boot PML4）。
pub fn init_high_map() {
    // 高 PDPT 用固定物理地址 0x7000（紧跟 boot 页表 0x4000-0x6FFF 之后）。
    // 不能走 get_free_page：它依赖空闲链表，而空闲链表要在本函数之后、由
    // page_alloc::init 建链时用高半区映射（push_free）写。0x7000 低于
    // MIN_USABLE_PHYS(1MB)，永远不进空闲链表，恒等映射内直写安全。
    const HIGH_PDPT: usize = 0x7000;
    // SAFETY：启动早期，恒等映射（低 1GB）完好，直写物理地址安全。
    unsafe {
        core::ptr::write_bytes(HIGH_PDPT as *mut u8, 0, PAGE_SIZE);
        // 高 PDPT[0] -> boot PD(0x6000) | PRESENT | RW
        core::ptr::write_volatile(HIGH_PDPT as *mut u64, 0x6000u64 | flags::PRESENT | flags::RW);
        // boot PML4(0x4000) 的 PML4[256] -> 高 PDPT | PRESENT | RW
        core::ptr::write_volatile(
            ((0x4000usize + 256 * 8) as *mut u64),
            (HIGH_PDPT as u64) | flags::PRESENT | flags::RW,
        );
    }
    invalidate();
}

/// 释放一个用户进程地址空间的全部用户页与其页表页（PDPT/PD/PT）。
///
/// 遍历 PML4 低半区（0..256，用户空间），只处理带 USER 位的页表分支：
/// - 末级 PTE 带 PRESENT|USER：这是真正的用户页，`page_ref_dec` 到 0 才
///   `free_page`（COW 共享页引用计数 >1 时只减不还）。
/// - RESERVED 叶（惰性分配、PRESENT=0）：无物理页，跳过。
/// - 恒等映射（无 USER 位）与高半区直接映射（PML4[256]，不在 0..256）不碰。
///
/// 调用时机：进程退出（release）与 execve 换地址空间（换掉 old_pml4 前）。
/// 之前这里只 `free_page(pml4)` 回收 PML4 页自身，PDPT/PD/PT 与用户物理页
/// 全部泄漏——gcc/g++ 连跑几个 exec 就把 256MB 空闲页耗尽，ld 加载库时 OOM。
///
/// # Safety
/// `pml4` 必须是一个已不再被当前 CR3 使用的用户 PML4 物理地址。
pub unsafe fn free_user_pages(pml4: usize) {
    use crate::mm::page_ref;
    // 只遍历 PML4 低半区（0..256 = 用户空间），高半区直接映射在 [256] 不碰。
    // 中间级（PML4/PDPT/PD）不按 USER 位过滤：ELF 装在低 0x400000，走的是
    // clone_kernel_pdpt 建的恒等映射副本（PML4[0]/PDPT[0] 无 USER 位），按 USER
    // 过滤会把整个 ELF 分支跳过。真正区分「用户页 vs 恒等映射」在末级 PTE：
    // 恒等映射 2MB 大页靠 HUGE 跳过，4KB 条目靠 USER 位区分。
    //
    // 共享的内核 MMIO 子树（boot PDPT[3]，clone_kernel_pdpt 把它原样挂进
    // 每个用户 PML4）：LAPIC/e1000/fb 的页表页属内核所有，绝不能随进程
    // 地址空间释放——否则 LAPIC 映射被拆，this_cpu 读 APIC ID 直接递归
    // 缺页挂死（实测：pid2 execve 换地址空间时把 LAPIC PT 页送进了空闲
    // 链表，页被二次分配后 LAPIC 全系统不可读）。
    let mmio_root = unsafe { entry(0x5000, 3) };
    for pml4_i in 0..256usize {
        let pml4e = unsafe { entry(pml4, pml4_i) };
        if pml4e & flags::PRESENT == 0 { continue; }
        if pml4e & flags::HUGE != 0 { continue; }
        let pdpt = (pml4e & ADDR_MASK) as usize;
        for pdpt_i in 0..512usize {
            let pdpte = unsafe { entry(pdpt, pdpt_i) };
            if pdpte & flags::PRESENT == 0 { continue; }
            if pdpte & flags::HUGE != 0 { continue; }
            if mmio_root != 0 && pdpte == mmio_root { continue; }
            let pd = (pdpte & ADDR_MASK) as usize;
            for pd_i in 0..512usize {
                let pde = unsafe { entry(pd, pd_i) };
                if pde & flags::PRESENT == 0 { continue; }
                if pde & flags::HUGE != 0 { continue; }
                let pt = (pde & ADDR_MASK) as usize;
                for pt_i in 0..512usize {
                    let pte = unsafe { entry(pt, pt_i) };
                    if pte & flags::RESERVED != 0 { continue; }
                    if pte & flags::PRESENT == 0 || pte & flags::USER == 0 { continue; }
                    let phys = (pte & ADDR_MASK) as usize;
                    let pfn = page_ref::phys_to_pfn(phys);
                    if page_ref::page_ref_dec(pfn) == 0 {
                        crate::mm::page_alloc::free_page(phys);
                    }
                }
                crate::mm::page_alloc::free_page(pt);
            }
            crate::mm::page_alloc::free_page(pd);
        }
        crate::mm::page_alloc::free_page(pdpt);
    }
}

/// 复制页表项到新的页表。
///
/// 复制源页表的用户空间映射到目标页表，适用于 fork 时的页表复制。
/// `user` 参数控制是否将 USER 位写入新条目。
///
/// **注意**：当前实现对 user=true 时检查 PTE 的权限标志有精简——只检查了
/// PML4/PDPT/PD 都存在的条目。对 2MB 大页直接跳过（不拆分）。
/// COW 语义留给调用者通过 `cow_copy_page_table` 处理。
///
/// # Safety
/// - src_pml4 和 dst_pml4 必须是有效的四级页表根
/// - 此函数不处理 COW 标记，需要调用者设置
pub unsafe fn copy_page_table(src_pml4: usize, dst_pml4: usize, user: bool) -> bool {
    // 用户空间范围: 1GB - 4GB（与 umm 的 USERSPACE_START/END 一致）
    const USERSPACE_START: usize = 0x4000_0000;
    const USERSPACE_END: usize = 0xFFFF_FFFF;

    let extra = if user { flags::USER } else { 0 };

    // 遍历用户空间的每一页
    let mut vaddr = USERSPACE_START;
    while vaddr < USERSPACE_END {
        // 检查源页表中的映射
        if let Some(phys) = unsafe { translate(src_pml4, vaddr) } {
            // 用 get_page_flags 在 src_pml4 上取标志——这是正确路径
            let prot = get_page_flags(src_pml4, vaddr).unwrap_or(flags::READONLY & !flags::USER);

            // 在目标页表中创建映射
            if !map_page(dst_pml4, vaddr, phys, prot | flags::PRESENT | extra) {
                return false;
            }
        }
        vaddr += PAGE_SIZE;
    }
    true
}

/// 复制并设置 COW 页表。
///
/// 对应原版 `mm/memory.c:copy_page_tables()`：遍历源进程的用户页表，
/// 把每一页用户页在父子两边都改成只读，并递增该物理页的引用计数
/// （原版 `mem_map[MAP_NR(page)]++`）。这样写时复制缺页处理程序才能
/// 依靠「引用计数 > 1 → 复制」正确工作。
///
/// 与旧实现的关键差别：旧实现按 `0x4000_0000..0xFFFF_FFFF` 线性扫描虚拟
/// 地址，但 BusyBox/glibc 把 ELF 装在 `0x400000` 段（低于 1GB），整段
/// 文本/数据都被跳过，fork 后子进程没有代码映射，一返回用户态即缺页。
/// 这里改为遍历页表树本身——只复制 PML4 低半区（用户空间，PML4[0..256]）
/// 里实际存在的条目，覆盖从 0 起的全部规范用户地址。
///
/// # Safety
/// - `src_pml4` 和 `dst_pml4` 必须是有效的四级页表根物理地址。
/// - `dst_pml4` 应已通过 [`clone_kernel_pdpt`] 建好内核映射。
/// - 调用方须保证独占（单核 + 关中断或不可重入上下文）。
pub unsafe fn cow_copy_page_table(src_pml4: usize, dst_pml4: usize) -> bool {
    use crate::mm::page_ref;

    // 用户空间 = PML4 低半区（index 0..256），对应规范地址 0..0x7FFF_FFFF_FFFF。
    // 高半区（256..512）是内核空间，由 clone_kernel_pdpt 处理，不在此复制。
    for pml4_i in 0..256usize {
        let pml4e = unsafe { entry(src_pml4, pml4_i) };
        if pml4e & flags::PRESENT == 0 || pml4e & flags::USER == 0 {
            continue;
        }
        // 源 PDPT 物理地址。
        let src_pdpt = (pml4e & ADDR_MASK) as usize;

        for pdpt_i in 0..512usize {
            let pdpte = unsafe { entry(src_pdpt, pdpt_i) };
            if pdpte & flags::PRESENT == 0 || pdpte & flags::USER == 0 {
                continue;
            }
            if pdpte & flags::HUGE != 0 {
                // 1GB 大页：用户空间不该出现（setup.S 的 1GB 大页无 USER 位）。
                // 出现说明状态异常，跳过以免误把内核巨页复制成用户页。
                continue;
            }
            let src_pd = (pdpte & ADDR_MASK) as usize;

            for pd_i in 0..512usize {
                let pde = unsafe { entry(src_pd, pd_i) };
                if pde & flags::PRESENT == 0 || pde & flags::USER == 0 {
                    continue;
                }
                if pde & flags::HUGE != 0 {
                    // 2MB 大页：用户空间也不该出现（next_level 在需要 4KB 粒度时
                    // 已把大页拆成 4KB 表）。出现则跳过。
                    continue;
                }
                let src_pt = (pde & ADDR_MASK) as usize;

                for pt_i in 0..512usize {
                    let pte = unsafe { entry(src_pt, pt_i) };
                    // 惰性保留页（PRESENT=0 + RESERVED）：fork 时子进程也要保留
                    // 同一段虚拟空间，否则子进程的 brk 堆/匿名映射区在 fork 后
                    // 直接「消失」，malloc 等一访问就是未映射的 SIGSEGV/野指针。
                    if pte & flags::RESERVED != 0 {
                        let vaddr = (pml4_i << 39) | (pdpt_i << 30)
                            | (pd_i << 21) | (pt_i << 12);
                        let prot = pte & (flags::RW | flags::USER | flags::NO_EXEC);
                        if !unsafe { map_reserved(dst_pml4, vaddr, prot) } {
                            return false;
                        }
                        continue;
                    }
                    // 已换出页（PRESENT=0 + SWAPPED）：父子共享同一 swap 槽，
                    // 槽位引用计数 +1（原版 swap_duplicate()）。
                    if pte & flags::SWAPPED != 0 {
                        let vaddr = (pml4_i << 39) | (pdpt_i << 30)
                            | (pd_i << 21) | (pt_i << 12);
                        crate::mm::swap::slot_ref_inc((pte >> 12) as u32);
                        if !unsafe { set_leaf_entry(dst_pml4, vaddr, pte) } {
                            return false;
                        }
                        continue;
                    }
                    if pte & flags::PRESENT == 0 || pte & flags::USER == 0 {
                        continue;
                    }
                    // 重建该 PTE 的虚拟地址。
                    let vaddr = (pml4_i << 39) | (pdpt_i << 30)
                        | (pd_i << 21) | (pt_i << 12);
                    // COW 只清 RW（bit 1）触发写保护缺页，其余标志（含 NX bit 63、
                    // ACCESSED/DIRTY 等）原样保留——原版 `copy_page_tables` 用
                    // `~2` 清 PAGE_RW，再 OR 上 PAGE_PRESENT|PAGE_USER。
                    let cow_flags = (pte & !ADDR_MASK & !flags::RW) | flags::PRESENT | flags::USER;

                    let phys = (pte & ADDR_MASK) as usize;
                    let pfn = page_ref::phys_to_pfn(phys);

                    // 原版：mem_map[MAP_NR(page)]++ —— 记录这一页现在被两个进程共享。
                    page_ref::page_ref_inc(pfn);

                    // 父进程：清 RW（变只读），保留其余标志（含 NX）。
                    // SAFETY: vaddr 在源页表里确实存在（上面四级都 PRESENT）。
                    unsafe {
                        let _ = set_page_flags(src_pml4, vaddr, cow_flags);
                    }

                    // 子进程：映射同一物理页，同样只读。
                    // SAFETY: dst_pml4 已建好内核半区，用户半区由本函数填充。
                    if !unsafe { map_page(dst_pml4, vaddr, phys, cow_flags) } {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// 获取页表项的权限标志
///
/// 返回 PTE 中除物理地址（bits 12–51）外的全部位：低位保护标志
/// （PRESENT/RW/USER/...）以及高位标志（NX bit 63、AVL bits 52–62）。
/// 旧实现用 `e & 0xFFF` 只取低 12 位，丢掉了 NX，导致 mprotect 与取指
/// 缺页处理无法正确判别/清除 NO_EXEC。
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

        // 物理地址占 bits 12–51；其余位都是标志（含 NX bit 63）。
        Some(e & !ADDR_MASK)
    }
}

/// 设置页表项的权限
///
/// `new_flags` 应为完整的标志位集合（建议用 [`get_page_flags`] 取出再修改）：
/// 物理地址（bits 12–51）从现有 PTE 取，其余位用 `new_flags`。高位标志
/// （如 NX bit 63）会按 `new_flags` 写入——传入则置位，不传（为 0）则清除。
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
