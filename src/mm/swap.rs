//! 交换（swap）子系统。对应 linux-1.0.9 的 `mm/swap.c`（`swapon`/
//! `swapoff`/`swap_out`/`swap_in` + `swap_bitmap`），简化版：
//!
//! - 交换区是一个块设备（`swapon /dev/hd98` 之类），块大小 1024 字节，
//!   一个 swap 槽 = 一页 = 4 个连续块；第 0 页留作头（签名
//!   "SHITIXSW" + 槽位数），数据从第 1 页起。
//! - PTE 编码：PRESENT=0 + SWAPPED(bit 10)，bits 12+ 存槽号，
//!   RW/USER/NO_EXEC 权限位原样保留在低位，swap-in 时恢复。
//! - 换出策略：`get_free_page` 失败且 swap 活动时，从当前任务的用户
//!   页里挑第一个「PRESENT|USER|RW 且引用计数 <=1」的页写出去
//!   （原版 1.0.9 的 `swap_out` 也是线性扫页表找可换页）。
//! - 换入：page fault 看到 SWAPPED 叶子 → 分配物理页、从槽位读回、
//!   恢复权限、释放槽位。
//! - fork：COW 复制页表时连 SWAPPED 项一起复制并给槽位加引用计数；
//!   进程退出时 `free_task_swap` 把还指向 swap 的槽位计数归零。
//!
//! 已知简化：没有 LRU/aging（挑第一个可换页）；没有 swap cache
//! （换入立即释放槽位）；不换出只读/共享页。

use super::page::PAGE_SIZE;
use super::paging::{self, flags};
use crate::fs::buffer;

/// 交换区签名（第 0 页开头 8 字节）。
const SWAP_MAGIC: &[u8; 8] = b"SHITIXSW";
/// 一个 swap 槽占的 1024 字节块数（1 页）。
const BLOCKS_PER_SLOT: u32 = (PAGE_SIZE / 1024) as u32;
/// 最大槽数（= 最大 swap 容量 4096 页 = 16MB）。
pub const MAX_SLOTS: usize = 4096;

/// 槽位引用计数：0 = 空闲，>0 = 被多少个 PTE 指向（fork 共享）。
static mut SWAP_MAP: [u8; MAX_SLOTS] = [0; MAX_SLOTS];
/// 交换区设备号与容量（槽数）。active=false 时全部接口是 no-op。
static mut SWAP_DEV: u16 = 0;
static mut NR_SLOTS: u32 = 0;
static mut SWAP_ACTIVE: bool = false;
/// 换出指针（轮转起点，避免总从页表头扫到同一个进程的热页）。
static mut SWAP_SCAN_HINT: usize = 0;

pub fn is_active() -> bool {
    // SAFETY: 单核启动期写、之后只读标志位。
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SWAP_ACTIVE)) }
}

/// 当前空闲槽数（自检用）。
pub fn nr_free_slots() -> u32 {
    let map = unsafe { &*core::ptr::addr_of!(SWAP_MAP) };
    let n = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(NR_SLOTS)) };
    map[..n as usize].iter().filter(|&&c| c == 0).count() as u32
}

/// 交换区总槽数（sysinfo 用）。
pub fn total_slots() -> u32 {
    // SAFETY: swapon 后只读。
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(NR_SLOTS)) }
}

/// 在块设备上启用交换区。对应原版 `sys_swapon()`。
///
/// 第 0 页写入签名头；数据槽位 1..=nr_slots。重复 swapon 报 EBUSY。
pub fn swapon_dev(dev: u16, total_blocks: u32) -> i64 {
    use crate::klib::errno::{EBUSY, EINVAL, ENOMEM};
    if is_active() {
        return -(EBUSY as i64);
    }
    let total_pages = total_blocks / BLOCKS_PER_SLOT;
    if total_pages < 2 {
        return -(EINVAL as i64);
    }
    // 槽位号即页号（第 0 页是头，最后一个是槽 nr_pages-1）。
    let nr = core::cmp::min(total_pages - 1, MAX_SLOTS as u32);
    if nr == 0 {
        return -(ENOMEM as i64);
    }

    // 写签名头（块 0..3）。
    // SAFETY: 启动/进程上下文；getblk 可能睡，契约允许。
    unsafe {
        for b in 0..BLOCKS_PER_SLOT {
            let Some(bh) = buffer::getblk(dev, b, 1024) else {
                return -(EINVAL as i64);
            };
            let data = buffer::bh(bh).data_mut();
            if b == 0 {
                data[..8].copy_from_slice(SWAP_MAGIC);
                data[8..12].copy_from_slice(&nr.to_le_bytes());
                data[12..].fill(0);
            } else {
                data.fill(0);
            }
            buffer::mark_buffer_dirty(bh);
            buffer::brelse(bh);
        }
        buffer::sync_dev(dev);

        (*core::ptr::addr_of_mut!(SWAP_MAP)).fill(0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SWAP_DEV), dev);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(NR_SLOTS), nr);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SWAP_ACTIVE), true);
    }
    crate::pr_info!("swap: on dev {:#x}, {} slots ({} KB)", dev, nr, nr * 4);
    0
}

/// 关闭交换区。对应原版 `sys_swapoff()`。有页还在交换区里时报 EBUSY。
pub fn swapoff_dev() -> i64 {
    use crate::klib::errno::{EBUSY, EINVAL};
    if !is_active() {
        return -(EINVAL as i64);
    }
    if nr_free_slots() != unsafe { core::ptr::read_volatile(core::ptr::addr_of!(NR_SLOTS)) } {
        return -(EBUSY as i64);
    }
    // SAFETY: 单核、无并发换页活动。
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SWAP_ACTIVE), false);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SWAP_DEV), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(NR_SLOTS), 0);
    }
    crate::pr_info!("swap: off");
    0
}

/// 分配一个空闲槽（引用计数置 1）。
fn alloc_slot() -> Option<u32> {
    // SAFETY: 换页路径都在关中断或单核进程上下文里。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let n = core::ptr::read_volatile(core::ptr::addr_of!(NR_SLOTS));
        let map = &mut *core::ptr::addr_of_mut!(SWAP_MAP);
        let mut found = None;
        for i in 0..n as usize {
            if map[i] == 0 {
                map[i] = 1;
                found = Some(i as u32);
                break;
            }
        }
        crate::irq::restore_flags(flags);
        found
    }
}

/// 槽位引用计数 +1（fork 复制 SWAPPED 页表项时）。
pub fn slot_ref_inc(slot: u32) {
    // SAFETY: 同 alloc_slot。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let map = &mut *core::ptr::addr_of_mut!(SWAP_MAP);
        if (slot as usize) < map.len() && map[slot as usize] > 0 {
            map[slot as usize] = map[slot as usize].saturating_add(1);
        }
        crate::irq::restore_flags(flags);
    }
}

/// 槽位引用计数 -1，减到 0 释放。
fn slot_ref_dec(slot: u32) {
    // SAFETY: 同 alloc_slot。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let map = &mut *core::ptr::addr_of_mut!(SWAP_MAP);
        if (slot as usize) < map.len() && map[slot as usize] > 0 {
            map[slot as usize] -= 1;
        }
        crate::irq::restore_flags(flags);
    }
}

/// 把一页物理内存写入槽位。
fn write_slot(slot: u32, phys: usize) -> bool {
    let dev = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SWAP_DEV)) };
    let base = (slot + 1) * BLOCKS_PER_SLOT; // +1: 第 0 页是头
    // SAFETY: phys 是有效整页；getblk/brelse 是缓冲缓存契约。
    unsafe {
        for i in 0..BLOCKS_PER_SLOT {
            let Some(bh) = buffer::getblk(dev, base + i, 1024) else { return false };
            buffer::bh(bh).data_mut().copy_from_slice(core::slice::from_raw_parts(
                (paging::PHYS_MAP_BASE + phys + i as usize * 1024) as *const u8,
                1024,
            ));
            buffer::mark_buffer_dirty(bh);
            buffer::brelse(bh);
        }
        buffer::sync_dev(dev);
    }
    true
}

/// 把槽位内容读回一页物理内存。
fn read_slot(slot: u32, phys: usize) -> bool {
    let dev = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SWAP_DEV)) };
    let base = (slot + 1) * BLOCKS_PER_SLOT;
    // SAFETY: 同 write_slot。
    unsafe {
        for i in 0..BLOCKS_PER_SLOT {
            let Some(bh) = buffer::bread(dev, base + i, 1024) else { return false };
            core::slice::from_raw_parts_mut(
                (paging::PHYS_MAP_BASE + phys + i as usize * 1024) as *mut u8,
                1024,
            )
            .copy_from_slice(buffer::bh(bh).data());
            buffer::brelse(bh);
        }
    }
    true
}

/// 换出指定虚拟页：内容写进一个 swap 槽，PTE 改成 SWAPPED 项，
/// 物理页还给伙伴系统。成功返回 true。
///
/// # Safety
/// `pml4` 是当前任务的有效页表根；进程上下文（I/O 会睡）。
pub unsafe fn swap_out_addr(pml4: usize, vaddr: usize) -> bool {
    let page_va = vaddr & !(PAGE_SIZE - 1);
    // SAFETY: 契约由调用者保证。
    unsafe {
        let Some(leaf) = paging::leaf_entry(pml4, page_va) else { return false };
        // 只换「存在、用户、可写、独占」的页：只读页可能与文件/零页关联，
        // 共享页换出去会让另一个引用者看到内容凭空消失。
        if leaf & flags::PRESENT == 0 || leaf & flags::USER == 0 || leaf & flags::RW == 0 {
            return false;
        }
        let phys = (leaf & paging::ADDR_MASK) as usize;
        let pfn = super::page_ref::phys_to_pfn(phys);
        if super::page_ref::page_ref_count(pfn) > 1 {
            return false;
        }
        let Some(slot) = alloc_slot() else { return false };
        if !write_slot(slot, phys) {
            slot_ref_dec(slot);
            return false;
        }
        let prot = leaf & (flags::RW | flags::USER | flags::NO_EXEC);
        let entry = ((slot as u64) << 12) | flags::SWAPPED | prot;
        paging::set_leaf_entry(pml4, page_va, entry);
        super::page_alloc::free_page(phys);
        crate::pr_debug!("swap: out va={:#x} slot={}", page_va, slot);
    }
    true
}

/// 重入保护：reclaim 里要跑块 I/O，I/O 路径若又触发 get_free_page OOM
/// 回收会无限递归。
static mut SWAP_RECLAIMING: bool = false;

/// 从当前任务挑一个可换页换出去，腾出至少一页物理内存。
/// 对应原版 `mm/swap.c:swap_out()` 的线性扫描（简化：只扫当前任务，
/// 从 SWAP_SCAN_HINT 轮转位置开始）。
///
/// # Safety
/// 进程上下文（I/O 会睡）。
pub unsafe fn reclaim_once() -> bool {
    if !is_active() {
        return false;
    }
    // SAFETY: 关中断读写标志，防 I/O 路径重入。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let r = core::ptr::addr_of_mut!(SWAP_RECLAIMING);
        if core::ptr::read_volatile(r) {
            crate::irq::restore_flags(flags);
            return false;
        }
        core::ptr::write_volatile(r, true);
        crate::irq::restore_flags(flags);
    }
    let task = crate::sched::current_index();
    // SAFETY: 当前任务指针有效。
    let pml4 = unsafe { (*crate::sched::task_ptr(task)).pml4 };
    let mut done = false;
    if pml4 != 0 {
        // SAFETY: pml4 有效；只读遍历 + 命中的叶子交给 swap_out_addr。
        done = unsafe { reclaim_scan(pml4) };
    }
    // SAFETY: 与入口处的置位对应。
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SWAP_RECLAIMING), false);
    }
    done
}

/// [`reclaim_once`] 的扫描主体。
///
/// # Safety
/// `pml4` 有效。
unsafe fn reclaim_scan(pml4: usize) -> bool {
    // SAFETY: 只读遍历。
    unsafe {
        let start = core::ptr::read_volatile(core::ptr::addr_of!(SWAP_SCAN_HINT));
        for round in 0..256usize {
            let pml4_i = (start + round) % 256;
            let pml4e = paging::entry(pml4, pml4_i);
            if pml4e & flags::PRESENT == 0 || pml4e & flags::USER == 0 {
                continue;
            }
            let pdpt = (pml4e & paging::ADDR_MASK) as usize;
            for pdpt_i in 0..512usize {
                let pdpte = paging::entry(pdpt, pdpt_i);
                if pdpte & flags::PRESENT == 0 || pdpte & flags::USER == 0
                    || pdpte & flags::HUGE != 0
                {
                    continue;
                }
                let pd = (pdpte & paging::ADDR_MASK) as usize;
                for pd_i in 0..512usize {
                    let pde = paging::entry(pd, pd_i);
                    if pde & flags::PRESENT == 0 || pde & flags::USER == 0
                        || pde & flags::HUGE != 0
                    {
                        continue;
                    }
                    let pt = (pde & paging::ADDR_MASK) as usize;
                    for pt_i in 0..512usize {
                        let pte = paging::entry(pt, pt_i);
                        if pte & flags::PRESENT == 0 || pte & flags::USER == 0
                            || pte & flags::RW == 0
                        {
                            continue;
                        }
                        let vaddr = (pml4_i << 39) | (pdpt_i << 30) | (pd_i << 21) | (pt_i << 12);
                        if swap_out_addr(pml4, vaddr) {
                            core::ptr::write_volatile(
                                core::ptr::addr_of_mut!(SWAP_SCAN_HINT),
                                pml4_i,
                            );
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// 缺页换入：fault_addr 的叶子是 SWAPPED 项时，分配物理页读回内容并
/// 恢复映射。返回 Some(true) 已换入；Some(false) 是 SWAPPED 但失败
/// （OOM/读错误）；None 不是 SWAPPED 项。
///
/// # Safety
/// 进程上下文的 page fault handler 里调用（I/O 会睡）。
pub unsafe fn try_swap_in(pml4: usize, fault_addr: usize) -> Option<bool> {
    let page_va = fault_addr & !(PAGE_SIZE - 1);
    // SAFETY: pml4 有效。
    let leaf = unsafe { paging::leaf_entry(pml4, page_va) }?;
    if leaf & flags::PRESENT != 0 || leaf & flags::SWAPPED == 0 {
        return None;
    }
    let slot = (leaf >> 12) as u32;
    let prot = leaf & (flags::RW | flags::USER | flags::NO_EXEC);
    // SAFETY: 进程上下文。
    unsafe {
        let pg = super::page_alloc::get_free_page();
        if pg == 0 {
            return Some(false);
        }
        if !read_slot(slot, pg) {
            super::page_alloc::free_page(pg);
            return Some(false);
        }
        if !paging::map_page(pml4, page_va, pg, prot | flags::PRESENT | flags::USER) {
            super::page_alloc::free_page(pg);
            return Some(false);
        }
        slot_ref_dec(slot);
    }
    crate::pr_debug!("swap: in va={:#x} slot={}", page_va, slot);
    Some(true)
}

/// 进程退出：释放其页表里所有 SWAPPED 项占用的槽位。
/// 对应原版 `free_page_tables` 里的 `swap_free(entry)`。
///
/// # Safety
/// `pml4` 属于正在消亡的任务，独占。
pub unsafe fn free_task_swap(pml4: usize) {
    if pml4 == 0 || !is_active() {
        return;
    }
    // SAFETY: 只读遍历；命中槽位用 slot_ref_dec 归还。
    unsafe {
        for pml4_i in 0..256usize {
            let pml4e = paging::entry(pml4, pml4_i);
            if pml4e & flags::PRESENT == 0 || pml4e & flags::USER == 0 {
                continue;
            }
            let pdpt = (pml4e & paging::ADDR_MASK) as usize;
            for pdpt_i in 0..512usize {
                let pdpte = paging::entry(pdpt, pdpt_i);
                if pdpte & flags::PRESENT == 0 || pdpte & flags::USER == 0
                    || pdpte & flags::HUGE != 0
                {
                    continue;
                }
                let pd = (pdpte & paging::ADDR_MASK) as usize;
                for pd_i in 0..512usize {
                    let pde = paging::entry(pd, pd_i);
                    if pde & flags::PRESENT == 0 || pde & flags::USER == 0
                        || pde & flags::HUGE != 0
                    {
                        continue;
                    }
                    let pt = (pde & paging::ADDR_MASK) as usize;
                    for pt_i in 0..512usize {
                        let pte = paging::entry(pt, pt_i);
                        if pte & flags::PRESENT == 0 && pte & flags::SWAPPED != 0 {
                            slot_ref_dec((pte >> 12) as u32);
                        }
                    }
                }
            }
        }
    }
}

/// 自检：在 ramdisk 上 swapon，走「换出→页表项变形→换入→内容一致」
/// 全链路，再 swapoff。只能在进程上下文调（I/O 会睡）。
pub fn selftest() {
    let dev = crate::drivers::block::ramdisk::RAMDISK_DEV;
    let blocks = crate::drivers::block::ramdisk::RD_BLOCKS as u32;

    let mut ok = true;
    let r = swapon_dev(dev, blocks);
    if r != 0 {
        crate::kprintln!("swap: swapon FAILED ({})", r);
        return;
    }
    let free0 = nr_free_slots();

    // 造一个「用户页」：在内核页表上映射一页 USER|RW，写入特征数据。
    let va = 0x0000_6000_0000usize; // 用户半区里一块测试地址
    let phys = super::page_alloc::get_free_page();
    if phys == 0 {
        crate::kprintln!("swap: selftest alloc FAILED");
        return;
    }
    // SAFETY: va 位于用户半区且当前未映射；phys 是刚分配的整页。
    unsafe {
        if !paging::map_page(0x4000, va, phys, flags::PRESENT | flags::USER | flags::RW) {
            super::page_alloc::free_page(phys);
            crate::kprintln!("swap: selftest map FAILED");
            return;
        }
        let p = (paging::PHYS_MAP_BASE + phys) as *mut u64;
        for i in 0..PAGE_SIZE / 8 {
            core::ptr::write_volatile(p.add(i), 0x5A5A_0000 + i as u64);
        }
        // 换出
        if !swap_out_addr(0x4000, va) {
            ok = false;
        } else {
            // 页表项应变形成 SWAPPED 且 translate 不到
            let le = paging::leaf_entry(0x4000, va);
            ok = ok && matches!(le, Some(e) if e & flags::PRESENT == 0 && e & flags::SWAPPED != 0);
            ok = ok && paging::translate(0x4000, va).is_none();
            ok = ok && nr_free_slots() == free0 - 1;
            // 换入
            match try_swap_in(0x4000, va) {
                Some(true) => {}
                _ => ok = false,
            }
            // 内容应与换出前一致
            if let Some(new_phys) = paging::translate(0x4000, va) {
                let np = (paging::PHYS_MAP_BASE + new_phys) as *const u64;
                for i in 0..PAGE_SIZE / 8 {
                    if core::ptr::read_volatile(np.add(i)) != 0x5A5A_0000 + i as u64 {
                        ok = false;
                        break;
                    }
                }
            } else {
                ok = false;
            }
            ok = ok && nr_free_slots() == free0;
        }
        // 清理测试映射
        if let Some(pp) = paging::unmap_page(0x4000, va) {
            super::page_alloc::free_page(pp);
        }
    }
    let off = swapoff_dev();
    ok = ok && off == 0;
    crate::kprintln!("swap: swapon/swapout/swapin/swapoff -> {}", if ok { "ok" } else { "FAIL" });
}
