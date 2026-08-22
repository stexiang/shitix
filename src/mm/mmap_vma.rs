//! mmap 的 per-task VMA 记账与惰性文件页缺页解析。
//!
//! 对应现代内核 `mm/mmap.c` 的 vm_area_struct 列表（1.0.9 原版只有
//! `mmap` 里的即时 `read`，没有 VMA 概念）。本模块只记 file-backed
//! 映射——匿名映射本来就是「保留页首次触碰清零分配」，不需要记账。
//!
//! 用途：
//! - `mmap(file)` 不再 eager 读入：只建 RESERVED 叶子 + 记 VMA，
//!   物理页和文件内容等第一次访问时由 page fault 路径补
//!   （glibc 动态链接器 map libc 数 MB 文本，进程实际只触碰一小部分）。
//! - `munmap` 按区间摘除 VMA，fork 时整张表随 COW 页表一起克隆，
//!   exec/exit 时清空。

use crate::sched::NR_TASKS;

/// 每任务最多同时存活的 file-backed VMA 数。glibc 一个进程映射
/// libc/ld.so/libm 各 3-4 段，32 足够。
pub const MAX_VMAS: usize = 32;

/// VMA 后端类型。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VmaKind {
    /// 空槽。
    Empty,
    /// 文件映射：`sb` 是 super_block 槽位（挂载期间稳定），`ino` 是
    /// 文件系统内 inode 号，`offset` 是 VMA 起点对应的文件偏移。
    File { sb: usize, ino: u32, offset: u64 },
}

/// 一条 file-backed VMA。
#[derive(Clone, Copy)]
pub struct MmapVma {
    pub start: usize,
    pub end: usize, // 开区间
    /// 页表权限位（paging::flags::{RW,USER,NO_EXEC} 子集）。
    pub prot: u64,
    pub kind: VmaKind,
}

/// 空槽常量（调用方造缓冲用）。
pub const EMPTY: MmapVma = MmapVma {
    start: 0,
    end: 0,
    prot: 0,
    kind: VmaKind::Empty,
};

static mut VMA_TABLE: [[MmapVma; MAX_VMAS]; NR_TASKS] = [[EMPTY; MAX_VMAS]; NR_TASKS];

/// 登记一条 VMA。区间重叠或槽满返回 false。
pub fn add(task: usize, vma: MmapVma) -> bool {
    if task >= NR_TASKS || vma.start >= vma.end {
        crate::pr_warn!("vma add early: task={} nr_tasks={} start={:#x} end={:#x}",
                        task, NR_TASKS, vma.start, vma.end);
        return false;
    }
    // SAFETY: 单核无抢占的 syscall 上下文里调用；写整张表前关中断防 AP 并发。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let tbl = &mut (*core::ptr::addr_of_mut!(VMA_TABLE))[task];
        let mut slot = None;
        for (i, v) in tbl.iter().enumerate() {
            if v.kind == VmaKind::Empty {
                if slot.is_none() {
                    slot = Some(i);
                }
                continue;
            }
            // 拒绝与现有 VMA 重叠
            if vma.start < v.end && vma.end > v.start {
                crate::pr_warn!("vma add overlap: new=[{:#x},{:#x}) old=[{:#x},{:#x}) kind={:?}",
                                vma.start, vma.end, v.start, v.end, v.kind);
                crate::irq::restore_flags(flags);
                return false;
            }
        }
        match slot {
            Some(i) => {
                tbl[i] = vma;
                crate::irq::restore_flags(flags);
                true
            }
            None => {
                let n = tbl.iter().filter(|v| v.kind != VmaKind::Empty).count();
                crate::pr_warn!("vma table full: task={} n={} first=[{:#x},{:#x}) last=[{:#x},{:#x})",
                    task, n,
                    tbl.iter().find(|v| v.kind != VmaKind::Empty).map(|v| v.start).unwrap_or(0),
                    tbl.iter().find(|v| v.kind != VmaKind::Empty).map(|v| v.end).unwrap_or(0),
                    tbl.iter().rev().find(|v| v.kind != VmaKind::Empty).map(|v| v.start).unwrap_or(0),
                    tbl.iter().rev().find(|v| v.kind != VmaKind::Empty).map(|v| v.end).unwrap_or(0));
                crate::irq::restore_flags(flags);
                false
            }
        }
    }
}

/// 当前任务已登记的 VMA 数（调试）。
pub fn count(task: usize) -> usize {
    if task >= NR_TASKS {
        return 0;
    }
    // SAFETY: 只读。
    unsafe {
        (*core::ptr::addr_of!(VMA_TABLE))[task]
            .iter()
            .filter(|v| v.kind != VmaKind::Empty)
            .count()
    }
}

/// 更新被 [start, end) 整段覆盖的 VMA 的 prot（mprotect）。
/// 部分重叠的 VMA 不拆——叶子权限已由 mprotect 逐页改对，VMA prot
/// 只影响还没触碰的惰性页，保守跳过。
pub fn set_prot(task: usize, start: usize, end: usize, prot: u64) {
    if task >= NR_TASKS || start >= end {
        return;
    }
    // SAFETY: 同 add。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let tbl = &mut (*core::ptr::addr_of_mut!(VMA_TABLE))[task];
        for v in tbl.iter_mut() {
            if v.kind != VmaKind::Empty && v.start >= start && v.end <= end {
                v.prot = prot;
            }
        }
        crate::irq::restore_flags(flags);
    }
}

/// 摘除并返回 [start, end) 完整包含的 VMA（mremap 搬运用）。
/// 部分重叠的按 remove_range 语义裁剪但不随搬运走。
pub fn drain_range(task: usize, start: usize, end: usize, out: &mut [MmapVma]) -> usize {
    if task >= NR_TASKS || start >= end {
        return 0;
    }
    let mut n = 0;
    // SAFETY: 同 add。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let tbl = &mut (*core::ptr::addr_of_mut!(VMA_TABLE))[task];
        for v in tbl.iter_mut() {
            if v.kind == VmaKind::Empty {
                continue;
            }
            if v.start >= start && v.end <= end {
                if n < out.len() {
                    out[n] = *v;
                    n += 1;
                }
                *v = EMPTY;
            } else if v.start < end && v.end > start {
                // 部分重叠：只留区间外的部分（不拆槽，取较长一侧的
                // 简化——mremap 按整段搬，部分覆盖本就少见）
                if start > v.start {
                    v.end = start;
                } else {
                    if let VmaKind::File { sb, ino, offset } = v.kind {
                        v.kind = VmaKind::File {
                            sb,
                            ino,
                            offset: offset + (end - v.start) as u64,
                        };
                    }
                    v.start = end;
                }
            }
        }
        crate::irq::restore_flags(flags);
    }
    n
}

/// 查包含 `addr` 的 VMA。
pub fn find(task: usize, addr: usize) -> Option<MmapVma> {
    if task >= NR_TASKS {
        return None;
    }
    // SAFETY: 只读；VMA 内容拷贝返回，不持引用。
    unsafe {
        for v in (*core::ptr::addr_of!(VMA_TABLE))[task].iter() {
            if v.kind != VmaKind::Empty && addr >= v.start && addr < v.end {
                return Some(*v);
            }
        }
    }
    None
}

/// 摘除 [start, end) 覆盖的 VMA（munmap）。相交的被裁剪，整段被盖的删除。
pub fn remove_range(task: usize, start: usize, end: usize) {
    if task >= NR_TASKS || start >= end {
        return;
    }
    // SAFETY: 同 add。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let tbl = &mut (*core::ptr::addr_of_mut!(VMA_TABLE))[task];
        for i in 0..MAX_VMAS {
            let v = tbl[i];
            if v.kind == VmaKind::Empty || end <= v.start || start >= v.end {
                continue;
            }
            // 记录左残段（start 之前的部分）
            let left = if start > v.start {
                Some(MmapVma { end: start, ..v })
            } else {
                None
            };
            // 文件偏移随区间起点平移
            let right = if end < v.end {
                let mut r = v;
                if let VmaKind::File { sb, ino, offset } = v.kind {
                    r.kind = VmaKind::File {
                        sb,
                        ino,
                        offset: offset + (end - v.start) as u64,
                    };
                }
                r.start = end;
                Some(r)
            } else {
                None
            };
            match (left, right) {
                (Some(l), Some(r)) => {
                    // 中间被挖掉：左段留在原槽，右段找新槽
                    tbl[i] = l;
                    let mut placed = false;
                    for w in tbl.iter_mut() {
                        if w.kind == VmaKind::Empty {
                            *w = r;
                            placed = true;
                            break;
                        }
                    }
                    if !placed {
                        // 槽满只能丢右段（munmap 语义上允许丢记账：
                        // 该区间仍已被 unmap，只是之后 VMA 信息缺失）
                        crate::pr_warn!("mmap_vma: table full, dropped right split");
                    }
                }
                (Some(l), None) => tbl[i] = l,
                (None, Some(r)) => tbl[i] = r,
                (None, None) => tbl[i] = EMPTY,
            }
        }
        crate::irq::restore_flags(flags);
    }
}

/// fork：把父任务的 VMA 表整张克隆给子任务（COW 页表已含 RESERVED 页，
/// 子进程缺页时按自己的 VMA 副本独立补文件内容）。
pub fn clone_table(from: usize, to: usize) {
    if from >= NR_TASKS || to >= NR_TASKS || from == to {
        return;
    }
    // SAFETY: fork 期间两任务的表都归当前上下文独占。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let tbl = &mut *core::ptr::addr_of_mut!(VMA_TABLE);
        tbl[to] = tbl[from];
        crate::irq::restore_flags(flags);
    }
}

/// exec/exit：清空任务的 VMA 表。
pub fn clear(task: usize) {
    if task >= NR_TASKS {
        return;
    }
    // SAFETY: 同 add。
    unsafe {
        let flags = crate::irq::local_irq_save();
        (*core::ptr::addr_of_mut!(VMA_TABLE))[task] = [EMPTY; MAX_VMAS];
        crate::irq::restore_flags(flags);
    }
}

/// 统一的保留页落实入口：先按 file-backed VMA 读文件内容，
/// 不在任何 VMA 里则落成匿名零页。copy_to/from_user、缺页 handler
/// 都应走这里，别直接调 [`super::paging::resolve_reserved`]——
/// 那会把 file-backed 保留页落成零页、丢文件内容。
///
/// # Safety
/// 同 [`resolve_file_fault`]。
pub unsafe fn resolve_any(task: usize, pml4: usize, vaddr: usize) -> bool {
    match unsafe { resolve_file_fault(task, pml4, vaddr) } {
        Some(ok) => ok,
        None => unsafe { super::paging::resolve_reserved(pml4, vaddr) },
    }
}
/// RESERVED 状态时，分配物理页、按文件偏移读入内容（越过文件末尾的
/// 部分清零，POSIX 语义）、按 VMA 权限落映射。
///
/// 返回 `Some(true)` 已解决；`Some(false)` 在 VMA 里但解析失败（OOM/
/// 读错误）；`None` 不在任何 file VMA 里（调用方走匿名零页路径）。
///
/// # Safety
/// 进程上下文的 page fault handler 里调用；可能 bread 睡眠。
pub unsafe fn resolve_file_fault(task: usize, pml4: usize, fault_addr: usize) -> Option<bool> {
    let page_va = fault_addr & !(super::page::PAGE_SIZE - 1);
    let vma = find(task, page_va)?;
    let VmaKind::File { sb, ino, offset } = vma.kind else {
        return None;
    };
    if !super::paging::is_reserved(pml4, page_va) {
        return Some(false);
    }

    let pg = super::page_alloc::get_free_page();
    if pg == 0 {
        return Some(false);
    }
    // 读文件内容：file_off = vma.offset + (page_va - vma.start)。
    let file_off = offset + (page_va - vma.start) as u64;
    let buf = unsafe {
        core::slice::from_raw_parts_mut(
            (super::paging::PHYS_MAP_BASE + pg) as *mut u8,
            super::page::PAGE_SIZE,
        )
    };
    // SAFETY: buf 是刚分配的整页；read_inode_at 返回读取字节数，
    // 其余部分 get_free_page 已清零（BSS/EOF 之外读作 0）。
    let n = unsafe { crate::fs::read_write::read_inode_at(sb, ino, file_off, buf) };
    if n < 0 {
        super::page_alloc::free_page(pg);
        return Some(false);
    }

    let prot = vma.prot | super::paging::flags::PRESENT | super::paging::flags::USER;
    // SAFETY: pml4/页都有效；RESERVED 叶子被正式映射覆盖。
    if !unsafe { super::paging::map_page(pml4, page_va, pg, prot) } {
        super::page_alloc::free_page(pg);
        return Some(false);
    }
    Some(true)
}
