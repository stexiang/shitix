//! vmalloc：把不连续的物理页映射成连续的虚拟地址区间。
//!
//! 对应现代内核的 `mm/vmalloc.c`（1.0.9 没有；原版最接近的是
//! `mm/swap.c` 里的 `vmalloc` 雏形）。用途：需要大块连续虚拟内存但
//! 物理页可以离散的场合（大缓冲、模块空间）。
//!
//! 布局：`VMALLOC_BASE`（0xFFFF_C000_0000_0000）起的一段高半区虚拟
//! 地址，与恒等映射（低 1GB）和物理直映（PHYS_MAP_BASE 起 1GB）都不
//! 重叠。所有映射建在内核页表（物理 0x4000）上，flag 为 KERNEL
//! （PRESENT|RW，无 USER），用户态访问会正常 #PF。

use super::page::PAGE_SIZE;
use super::page_alloc::{free_page, get_free_page};
use super::paging;
use crate::irq;

/// vmalloc 虚拟地址区起点。
pub const VMALLOC_BASE: usize = 0xFFFF_C000_0000_0000;
/// 区域总大小（虚拟地址空间，512MB 足够本内核用）。
pub const VMALLOC_SIZE: usize = 512 * 1024 * 1024;
/// 最大同时存活的 vmalloc 区数。
const MAX_AREAS: usize = 64;

/// 每个已分配区的记账。物理页号不单独存：vfree 时对区间内每页
/// `translate` 反查物理地址再归还。
#[derive(Clone, Copy)]
struct AreaSlot {
    va: usize,
    npages: usize,
}

static mut AREAS: [AreaSlot; MAX_AREAS] = [AreaSlot { va: 0, npages: 0 }; MAX_AREAS];

/// 分配 `size` 字节的连续虚拟内存（按页取整），失败返回 0。
///
/// 物理页逐页 `get_free_page`（可以离散），虚拟区间在 VMALLOC 区找一段
/// 空闲 span。返回的内存已清零（get_free_page 清零）。
pub fn vmalloc(size: usize) -> usize {
    if size == 0 || size > VMALLOC_SIZE {
        return 0;
    }
    let npages = (size + PAGE_SIZE - 1) / PAGE_SIZE;

    // SAFETY: 全程关中断，独占 AREAS 表与页表（避免 AP/中断路径并发 vmalloc）。
    unsafe {
        let irq_flags = irq::local_irq_save();

        // 找空槽
        let slot = {
            let areas = &mut *core::ptr::addr_of_mut!(AREAS);
            let mut found = None;
            for i in 0..MAX_AREAS {
                if areas[i].va == 0 { found = Some(i); break; }
            }
            found
        };
        let Some(slot) = slot else {
            irq::restore_flags(irq_flags);
            return 0;
        };

        // 找空闲虚拟 span（按已有区排序试错）
        let span = find_free_span(npages);
        let Some(va) = span else {
            irq::restore_flags(irq_flags);
            return 0;
        };

        // 逐页分配并映射
        let kpgt = 0x4000usize; // 内核页表物理地址（setup.S 建）
        let mut mapped = 0usize;
        for i in 0..npages {
            let pg = get_free_page();
            if pg == 0
                || !paging::map_page(kpgt, va + i * PAGE_SIZE, pg, paging::flags::KERNEL)
            {
                // 回滚
                for k in 0..mapped {
                    let v = va + k * PAGE_SIZE;
                    if let Some(p) = paging::translate(kpgt, v) {
                        paging::unmap_page(kpgt, v);
                        free_page(p);
                    }
                }
                irq::restore_flags(irq_flags);
                return 0;
            }
            mapped += 1;
        }

        (*core::ptr::addr_of_mut!(AREAS))[slot] = AreaSlot { va, npages };
        irq::restore_flags(irq_flags);
        va
    }
}

/// 释放 `vmalloc` 返回的虚拟区间。
///
/// # Safety
/// `addr` 必须是一次 vmalloc 的原样返回值，且之后不再访问。
pub unsafe fn vfree(addr: usize) {
    if addr == 0 {
        return;
    }
    // SAFETY: 同 vmalloc，关中断独占。
    unsafe {
        let irq_flags = irq::local_irq_save();
        let areas = &mut *core::ptr::addr_of_mut!(AREAS);
        for i in 0..MAX_AREAS {
            if areas[i].va == addr {
                let kpgt = 0x4000usize;
                for k in 0..areas[i].npages {
                    let v = addr + k * PAGE_SIZE;
                    if let Some(p) = paging::translate(kpgt, v) {
                        paging::unmap_page(kpgt, v);
                        free_page(p);
                    }
                }
                areas[i].va = 0;
                areas[i].npages = 0;
                irq::restore_flags(irq_flags);
                return;
            }
        }
        irq::restore_flags(irq_flags);
    }
}

/// 在 VMALLOC 区里找一段能放 `npages` 页的连续虚拟空间。
///
/// # Safety
/// 调用者须持有 AREAS 的独占（关中断）。
unsafe fn find_free_span(npages: usize) -> Option<usize> {
    let areas = &*core::ptr::addr_of!(AREAS);
    let mut cand = VMALLOC_BASE;
    loop {
        let end = cand + npages * PAGE_SIZE;
        if end > VMALLOC_BASE + VMALLOC_SIZE {
            return None;
        }
        let mut conflict = false;
        for a in areas.iter() {
            if a.va != 0 && cand < a.va + a.npages * PAGE_SIZE && end > a.va {
                cand = a.va + a.npages * PAGE_SIZE;
                conflict = true;
                break;
            }
        }
        if !conflict {
            return Some(cand);
        }
    }
}

/// 自检：分配 3 页（跨页读写）、translate 校验、释放、复用。
pub fn selftest() {
    let va = vmalloc(3 * PAGE_SIZE);
    if va == 0 {
        crate::kprintln!("vmalloc: alloc FAILED");
        return;
    }
    let mut ok = va == VMALLOC_BASE; // 首个分配应落在区首
    // 跨页写读
    // SAFETY: va 是本函数刚 vmalloc 的 3 页区间，内核态可写。
    unsafe {
        for i in 0..3 * PAGE_SIZE / 8 {
            core::ptr::write_volatile((va as *mut u64).add(i), 0x5150_0000 + i as u64);
        }
        for i in 0..3 * PAGE_SIZE / 8 {
            if core::ptr::read_volatile((va as *const u64).add(i)) != 0x5150_0000 + i as u64 {
                ok = false;
                break;
            }
        }
        // 三页应映射到三个不同物理页
        let p0 = paging::translate(0x4000, va);
        let p1 = paging::translate(0x4000, va + PAGE_SIZE);
        let p2 = paging::translate(0x4000, va + 2 * PAGE_SIZE);
        ok = ok && p0.is_some() && p1.is_some() && p2.is_some()
            && p0 != p1 && p1 != p2 && p0 != p2;
        vfree(va);
        // 释放后 translate 应失败，且再分配应复用同一 VA
        ok = ok && paging::translate(0x4000, va).is_none();
        let va2 = vmalloc(PAGE_SIZE);
        ok = ok && va2 == va;
        vfree(va2);
    }
    crate::kprintln!("vmalloc: selftest -> {}", if ok { "ok" } else { "FAIL" });
}
