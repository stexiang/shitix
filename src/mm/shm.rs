//! SysV 共享内存。对应原版 `ipc/shm.c` 的简化移植：
//! 段表 + 附加表两张静态表（单核合作式调度，不需要 ipc 的读写信号量）。
//!
//! - `shmget(key, size, flags)`：按 key 查找/创建段（IPC_PRIVATE 恒新建）
//! - `shmat(id, addr, flags)`：把段的物理页映进当前任务页表
//! - `shmdt(addr)`：解除映射；nattch 归零且已 IPC_RMID 时释放物理页
//! - `shmctl(id, cmd, buf)`：IPC_STAT / IPC_RMID
//!
//! 已知限制：fork 的 COW 路径会把共享页当普通用户页处理（子进程写得到
//! 私有副本），不保证跨 fork 的共享语义——原版靠 `shm_vm_ops` 的
//! open/close 挂钩，本树没有 VMA 级 ops。

use crate::mm::page_alloc::{get_free_page, free_page};
use crate::mm::paging;
use crate::mm::PAGE_SIZE;
use crate::klib::errno::{EINVAL, EEXIST, ENOENT, ENOSPC, ENOMEM, EIDRM};

/// 最大段数（原版 SHMMNI=4096，本树够用即可）
const MAX_SEGS: usize = 16;
/// 单段最大页数（256 页 = 1MB）
const MAX_SHM_PAGES: usize = 256;
/// 最大附加数（所有任务合计）
const MAX_ATTACH: usize = 64;

/// IPC_CREAT / IPC_EXCL / IPC_PRIVATE
pub const IPC_CREAT: i32 = 0o1000;
pub const IPC_EXCL: i32 = 0o2000;
pub const IPC_PRIVATE: i32 = 0;

/// shmctl 命令
pub const IPC_RMID: i32 = 0;
pub const IPC_STAT: i32 = 2;

/// 一个共享内存段
#[derive(Clone, Copy)]
struct ShmSeg {
    used: bool,
    key: i32,
    size: usize,
    npages: usize,
    /// 各页的物理地址
    pages: [usize; MAX_SHM_PAGES],
    nattch: u32,
    /// IPC_RMID 后标记删除（最后 detach 时真正释放）
    destroy: bool,
}

const EMPTY_SEG: ShmSeg = ShmSeg {
    used: false, key: 0, size: 0, npages: 0,
    pages: [0; MAX_SHM_PAGES], nattch: 0, destroy: false,
};

/// 一次附加（某任务把某段映到了某虚拟地址）
#[derive(Clone, Copy)]
struct Attach {
    used: bool,
    task: usize,
    seg: usize,
    va: usize,
}

const EMPTY_ATTACH: Attach = Attach { used: false, task: 0, seg: 0, va: 0 };

static mut SEGS: [ShmSeg; MAX_SEGS] = [EMPTY_SEG; MAX_SEGS];
static mut ATTACHES: [Attach; MAX_ATTACH] = [EMPTY_ATTACH; MAX_ATTACH];

/// 按 key 查段，返回段下标。
fn find_by_key(key: i32) -> Option<usize> {
    // SAFETY: 调用点都在临界区内。
    unsafe {
        for (i, s) in (*core::ptr::addr_of!(SEGS)).iter().enumerate() {
            if s.used && s.key == key && key != IPC_PRIVATE {
                return Some(i);
            }
        }
    }
    None
}

/// 释放段的全部物理页并把段槽清空。
fn free_seg(idx: usize) {
    // SAFETY: 调用点都在临界区内；段已确认 used。
    unsafe {
        let s = &mut (*core::ptr::addr_of_mut!(SEGS))[idx];
        for p in s.pages.iter().take(s.npages) {
            if *p != 0 {
                free_page(*p);
            }
        }
        *s = EMPTY_SEG;
    }
}

/// shmget(key, size, shmflg)。成功返回段 id（>=0）。
pub fn sys_shmget(key: i32, size: usize, shmflg: i32) -> i64 {
    if size == 0 || size > MAX_SHM_PAGES * PAGE_SIZE {
        return -(EINVAL as i64);
    }
    let npages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
    // SAFETY: 关中断的临界区内独占两张表。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let segs = &mut *core::ptr::addr_of_mut!(SEGS);
        let r = if let Some(idx) = find_by_key(key) {
            if shmflg & (IPC_CREAT | IPC_EXCL) == (IPC_CREAT | IPC_EXCL) {
                Err(EEXIST)
            } else {
                Ok(idx)
            }
        } else {
            if shmflg & IPC_CREAT == 0 && key != IPC_PRIVATE {
                Err(ENOENT)
            } else {
                match segs.iter().position(|s| !s.used) {
                    None => Err(ENOSPC),
                    Some(idx) => {
                        let mut ok = true;
                        let mut got = 0usize;
                        for slot in 0..npages {
                            let p = get_free_page();
                            if p == 0 { ok = false; break; }
                            segs[idx].pages[slot] = p;
                            got += 1;
                        }
                        if !ok {
                            for slot in 0..got {
                                free_page(segs[idx].pages[slot]);
                                segs[idx].pages[slot] = 0;
                            }
                            Err(ENOMEM)
                        } else {
                            segs[idx].used = true;
                            segs[idx].key = key;
                            segs[idx].size = size;
                            segs[idx].npages = npages;
                            segs[idx].nattch = 0;
                            segs[idx].destroy = false;
                            Ok(idx)
                        }
                    }
                }
            }
        };
        crate::irq::restore_flags(flags);
        match r {
            Ok(idx) => idx as i64,
            Err(e) => -(e as i64),
        }
    }
}

/// shmat(id, shmaddr, shmflg)。成功返回映射地址。
///
/// `pml4` 为当前任务页表；`pick_addr` 在 shmaddr==0 时由调用方提供
/// 空闲地址（通常按 mmap_base 向下排）。
pub unsafe fn sys_shmat(id: usize, shmaddr: usize, pml4: usize, pick_addr: impl Fn(usize) -> usize) -> i64 {
    // SAFETY: 临界区读段表。
    let (npages, pages_snapshot_ok, size) = unsafe {
        let flags = crate::irq::local_irq_save();
        let segs = &*core::ptr::addr_of!(SEGS);
        let r = if id >= MAX_SEGS || !segs[id].used || segs[id].destroy {
            None
        } else {
            Some((segs[id].npages, true, segs[id].size))
        };
        crate::irq::restore_flags(flags);
        match r {
            Some(v) => v,
            None => return -(EIDRM as i64),
        }
    };
    let _ = pages_snapshot_ok;

    let va = if shmaddr != 0 {
        shmaddr & !(PAGE_SIZE - 1)
    } else {
        pick_addr(npages * PAGE_SIZE)
    };
    if va == 0 {
        return -(ENOMEM as i64);
    }

    // 逐页映射进当前任务页表（USER|RW，数据段不可执行）
    let prot = paging::flags::USER | paging::flags::PRESENT | paging::flags::RW
        | paging::flags::NO_EXEC;
    // SAFETY: 临界区把页数组快照出来再映射，避免持锁进 map_page。
    let mut snap = [0usize; MAX_SHM_PAGES];
    unsafe {
        let flags = crate::irq::local_irq_save();
        let segs = &*core::ptr::addr_of!(SEGS);
        snap[..npages].copy_from_slice(&segs[id].pages[..npages]);
        crate::irq::restore_flags(flags);
    }
    for (i, p) in snap.iter().take(npages).enumerate() {
        // SAFETY: va 为用户地址、页框属于本段；失败回滚已映射页。
        if !unsafe { paging::map_page(pml4, va + i * PAGE_SIZE, *p, prot) } {
            for j in 0..i {
                unsafe { paging::unmap_page(pml4, va + j * PAGE_SIZE) };
            }
            return -(ENOMEM as i64);
        }
    }

    // 记附加并加计数
    let task = crate::sched::current_index();
    // SAFETY: 临界区写附加表。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let at = &mut *core::ptr::addr_of_mut!(ATTACHES);
        match at.iter().position(|a| !a.used) {
            None => {
                crate::irq::restore_flags(flags);
                for j in 0..npages {
                    paging::unmap_page(pml4, va + j * PAGE_SIZE);
                }
                return -(ENOMEM as i64);
            }
            Some(slot) => {
                at[slot] = Attach { used: true, task, seg: id, va };
                (*core::ptr::addr_of_mut!(SEGS))[id].nattch += 1;
                crate::irq::restore_flags(flags);
            }
        }
    }
    let _ = size;
    va as i64
}

/// shmdt(shmaddr)。成功返回 0。
pub unsafe fn sys_shmdt(shmaddr: usize, pml4: usize) -> i64 {
    let va = shmaddr & !(PAGE_SIZE - 1);
    let task = crate::sched::current_index();
    // SAFETY: 临界区查附加表。
    let found = unsafe {
        let flags = crate::irq::local_irq_save();
        let at = &*core::ptr::addr_of!(ATTACHES);
        let r = at.iter().position(|a| a.used && a.task == task && a.va == va);
        crate::irq::restore_flags(flags);
        r
    };
    let slot = match found {
        Some(s) => s,
        None => return -(EINVAL as i64),
    };
    let (seg, npages) = unsafe {
        let flags = crate::irq::local_irq_save();
        let at = &mut *core::ptr::addr_of_mut!(ATTACHES);
        let seg = at[slot].seg;
        at[slot] = EMPTY_ATTACH;
        let np = (*core::ptr::addr_of!(SEGS))[seg].npages;
        crate::irq::restore_flags(flags);
        (seg, np)
    };
    for i in 0..npages {
        // SAFETY: 解除本任务页表里的映射，不碰物理页（归段所有）。
        unsafe { paging::unmap_page(pml4, va + i * PAGE_SIZE) };
    }
    finish_detach(seg);
    0
}

/// nattch--；若段已标记删除且无附加，释放物理页。
fn finish_detach(seg: usize) {
    // SAFETY: 临界区独占段表。
    unsafe {
        let flags = crate::irq::local_irq_save();
        let segs = &mut *core::ptr::addr_of_mut!(SEGS);
        if segs[seg].nattch > 0 {
            segs[seg].nattch -= 1;
        }
        let release = segs[seg].destroy && segs[seg].nattch == 0;
        crate::irq::restore_flags(flags);
        if release {
            free_seg(seg);
        }
    }
}

/// shmctl(id, cmd, buf)。支持 IPC_STAT / IPC_RMID。
pub unsafe fn sys_shmctl(id: usize, cmd: i32, buf: *mut u8) -> i64 {
    match cmd {
        IPC_RMID => {
            // SAFETY: 临界区改段表。
            let release = unsafe {
                let flags = crate::irq::local_irq_save();
                let segs = &mut *core::ptr::addr_of_mut!(SEGS);
                if id >= MAX_SEGS || !segs[id].used {
                    crate::irq::restore_flags(flags);
                    return -(EINVAL as i64);
                }
                segs[id].destroy = true;
                let r = segs[id].nattch == 0;
                crate::irq::restore_flags(flags);
                r
            };
            if release {
                free_seg(id);
            }
            0
        }
        IPC_STAT => {
            if buf.is_null() {
                return -(EINVAL as i64);
            }
            // SAFETY: 临界区读段表。
            let (key, size, nattch) = unsafe {
                let flags = crate::irq::local_irq_save();
                let segs = &*core::ptr::addr_of!(SEGS);
                if id >= MAX_SEGS || !segs[id].used {
                    crate::irq::restore_flags(flags);
                    return -(EINVAL as i64);
                }
                let v = (segs[id].key, segs[id].size, segs[id].nattch);
                crate::irq::restore_flags(flags);
                v
            };
            // x86_64 struct shmid_ds：ipc_perm 48 字节
            //   key@0(i32) uid@4 gid@8 cuid@12 cgid@16 mode@20(u32)
            // 之后 shm_atime@48 dtime@56 ctime@64 segsz@72 cpid@80 lpid@84
            // nattch@88(u64)。先清零 112 字节再填字段。
            // SAFETY: 用户指针恒等映射可写。
            unsafe {
                core::ptr::write_bytes(buf, 0, 112);
                (buf as *mut i32).write_volatile(key);
                (buf.add(20) as *mut u32).write_volatile(0o666);
                (buf.add(72) as *mut u64).write_volatile(size as u64);
                (buf.add(88) as *mut u64).write_volatile(nattch as u64);
            }
            0
        }
        _ => -(EINVAL as i64),
    }
}

/// 任务退出时摘掉它的全部附加（do_exit 调用）。
pub fn exit_task(task: usize, pml4: usize) {
    loop {
        // SAFETY: 临界区查附加表。
        let found = unsafe {
            let flags = crate::irq::local_irq_save();
            let at = &*core::ptr::addr_of!(ATTACHES);
            let r = at.iter().position(|a| a.used && a.task == task);
            crate::irq::restore_flags(flags);
            r
        };
        let slot = match found {
            Some(s) => s,
            None => return,
        };
        let (seg, va, npages) = unsafe {
            let flags = crate::irq::local_irq_save();
            let at = &mut *core::ptr::addr_of_mut!(ATTACHES);
            let seg = at[slot].seg;
            let va = at[slot].va;
            at[slot] = EMPTY_ATTACH;
            let np = (*core::ptr::addr_of!(SEGS))[seg].npages;
            crate::irq::restore_flags(flags);
            (seg, va, np)
        };
        if pml4 != 0 {
            for i in 0..npages {
                // SAFETY: 解除将死任务页表里的映射。
                unsafe { paging::unmap_page(pml4, va + i * PAGE_SIZE) };
            }
        }
        finish_detach(seg);
    }
}
