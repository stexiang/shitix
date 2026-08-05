//! 内核小块内存分配器。
//!
//! 对应 linux-1.0.9 的 `mm/kmalloc.c`（作者 Rogier Wolff）。保留了原版的整套结构：
//!   - `sizes[]`：按块大小分档，每档一条页链表（32/64/128/252/508/1020/2040/4080）
//!   - `page_descriptor`：每页开头一个描述符，记录 order、空闲块数、页内空闲链表
//!   - `block_header`：每块前面一个头，存 `MF_USED`/`MF_FREE` 魔数与长度
//!   - `kfree` 时若整页空闲则把页还给页帧分配器
//!
//! 与原版的差异：
//!   - 原版 `block_header` 里 length 和 next 是 union（省 4 字节）；这里为了不写
//!     裸 union，用两个字段分别存，因此头从 8 字节变成 24 字节（含 64 位对齐）。
//!     块大小档位相应按 x86_64 重新算过，不再是原版那串 252/508/1020 的魔数。
//!   - 不实现原版的 `nmallocs`/`nbytesmalloced` 等统计（原版注释说这些只是
//!     "oooohhhhh, aaaaahhhhh" 给用户看的），只留 [`stats`] 需要的最小几项。

use super::page::{PAGE_SIZE, page_base};
use super::page_alloc::{free_page, get_free_page_raw};

/// 块已分配的魔数。同原版 `MF_USED`。
const MF_USED: u32 = 0xffaa_0055;
/// 块空闲的魔数。同原版 `MF_FREE`。
const MF_FREE: u32 = 0x0055_ffaa;

/// 单次 kmalloc 的上限。原版 `MAX_KMALLOC_K * 1024`，这里就是一页减去开销。
const MAX_ORDER: usize = 7;

/// 每块前面的头。对应原版 `struct block_header`。
#[repr(C)]
struct BlockHeader {
    flags: u32,
    /// 分配时记录请求长度（原版 `bh_length`），空闲时无意义
    length: usize,
    /// 空闲时指向同页下一个空闲块（原版 `bh_next`），0 表示末尾
    next: usize,
}

const HDR: usize = core::mem::size_of::<BlockHeader>();

/// 每页开头的描述符。对应原版 `struct page_descriptor`。
#[repr(C)]
struct PageDescriptor {
    /// 同档位的下一页（原版 `next`），0 表示末尾
    next: usize,
    /// 页内第一个空闲块（原版 `firstfree`），0 表示满
    firstfree: usize,
    order: usize,
    nfree: usize,
}

const DESC: usize = core::mem::size_of::<PageDescriptor>();

/// 由块指针求所在页的描述符地址。同原版宏 `PAGE_DESC(p)`。
#[inline]
fn page_desc_of(p: usize) -> usize {
    page_base(p)
}

/// 一个大小档位。对应原版 `struct size_descriptor`。
#[derive(Copy, Clone)]
struct SizeDescriptor {
    /// 该档位第一张有空闲块的页（原版 `firstfree`）
    firstfree: usize,
    /// 块大小（含 header）
    size: usize,
    /// 每页能放多少块
    nblocks: usize,
    npages: usize,
}

/// 档位表。原版是 32/64/128/252/508/1020/2040/4080 这串常量；
/// 这里保持「2 的幂 + 末档吃掉整页剩余」的形状，按 64 位 header 重算。
static mut SIZES: [SizeDescriptor; MAX_ORDER + 1] = {
    const fn d(size: usize) -> SizeDescriptor {
        SizeDescriptor { firstfree: 0, size, nblocks: (PAGE_SIZE - DESC) / size, npages: 0 }
    }
    [d(32), d(64), d(128), d(256), d(512), d(1024), d(2048), d(PAGE_SIZE - DESC)]
};

/// 取档位表。
///
/// # Safety
/// 调用者保证无并发访问（内核早期单线程；开中断后需 cli 保护，同原版）。
unsafe fn sizes() -> &'static mut [SizeDescriptor; MAX_ORDER + 1] {
    // SAFETY: SIZES 是常量初始化的静态数组，地址恒定有效；由调用者契约保证独占。
    unsafe { &mut *core::ptr::addr_of_mut!(SIZES) }
}

/// 请求长度 → 档位。对应原版 `get_order()`。
fn get_order(size: usize) -> Option<usize> {
    let need = size.checked_add(HDR)?;
    // SAFETY: 只读档位表的 size 字段，早期单线程。
    let tbl = unsafe { sizes() };
    (0..=MAX_ORDER).find(|&o| need <= tbl[o].size)
}

/// 分配 `size` 字节。对应原版 `kmalloc()`。失败返回空指针。
///
/// 原版有 `MAX_GET_FREE_PAGE_TRIES` 次重试（防其他进程抢走新页），
/// 这里单线程无竞争，拿到新页后必然能切出块，所以只走一轮。
pub fn kmalloc(size: usize) -> *mut u8 {
    let Some(order) = get_order(size) else {
        // 原版：printk("kmalloc of too large a block (%d bytes).\n", size)
        return core::ptr::null_mut();
    };

    // 原版 `kmalloc` 全程 `cli()`（它注释里明说 "This is a very simple
    // and stupid ... we have to be careful about interrupts"）。档位表的
    // `firstfree` 链和页内的空闲块链都是读—改—写，中断里也可能 kmalloc，
    // 交错会把同一个块派两次。
    // SAFETY: 全程关中断，独占档位表；页描述符与块头都在自己分配的页内。
    unsafe {
        let _guard = IrqGuard::new();
        let tbl = sizes();

        // 先看该档位有没有现成的空闲块。同原版第一段。
        if tbl[order].firstfree != 0 {
            let page = tbl[order].firstfree as *mut PageDescriptor;
            let p = (*page).firstfree;
            if p != 0 {
                let hdr = p as *mut BlockHeader;
                if (*hdr).flags != MF_FREE {
                    // 原版：printk("Problem: block on freelist isn't free.")
                    return core::ptr::null_mut();
                }
                (*page).firstfree = (*hdr).next;
                (*page).nfree -= 1;
                if (*page).nfree == 0 {
                    // 页满了，从档位链表摘掉
                    tbl[order].firstfree = (*page).next;
                    (*page).next = 0;
                }
                (*hdr).flags = MF_USED;
                (*hdr).length = size;
                return (p + HDR) as *mut u8;
            }
        }

        // 没有空闲块，要一张新页并切分。同原版第二段。
        let page_addr = get_free_page_raw();
        if page_addr == 0 {
            // 原版：printk("Couldn't get a free page.....\n")
            return core::ptr::null_mut();
        }
        let sz = tbl[order].size;
        let nblocks = tbl[order].nblocks;
        tbl[order].npages += 1;

        // 把页内块串成空闲链表。同原版那个 for 循环加"最后一块"的特判。
        let first = page_addr + DESC;
        for i in 0..nblocks {
            let p = first + i * sz;
            let hdr = p as *mut BlockHeader;
            (*hdr).flags = MF_FREE;
            (*hdr).length = 0;
            (*hdr).next = if i + 1 < nblocks { p + sz } else { 0 };
        }

        let page = page_addr as *mut PageDescriptor;
        (*page).order = order;
        (*page).nfree = nblocks;
        (*page).firstfree = first;
        (*page).next = tbl[order].firstfree;
        tbl[order].firstfree = page_addr;

        // 立刻从新页取第一块
        let hdr = first as *mut BlockHeader;
        (*page).firstfree = (*hdr).next;
        (*page).nfree -= 1;
        if (*page).nfree == 0 {
            tbl[order].firstfree = (*page).next;
            (*page).next = 0;
        }
        (*hdr).flags = MF_USED;
        (*hdr).length = size;
        (first + HDR) as *mut u8
    }
}

/// 分配并清零。
pub fn kzalloc(size: usize) -> *mut u8 {
    let p = kmalloc(size);
    if !p.is_null() {
        // SAFETY: kmalloc 返回的块至少有 size 字节可用，且当前只有我们持有它。
        unsafe { core::ptr::write_bytes(p, 0, size) }
    }
    p
}

/// 释放 `kmalloc` 返回的指针。对应原版 `kfree_s()`（`kfree` 是 `kfree_s(x, 0)`）。
///
/// # Safety
/// `ptr` 必须是 [`kmalloc`] / [`kzalloc`] 返回且尚未释放的指针。
pub unsafe fn kfree(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: 由调用者契约保证 ptr 来自 kmalloc，故其前 HDR 字节是合法的
    // BlockHeader，且所在页开头是我们写入的 PageDescriptor。
    unsafe {
        let _guard = IrqGuard::new();
        let p = ptr as usize - HDR;
        let hdr = p as *mut BlockHeader;
        let page = page_desc_of(p) as *mut PageDescriptor;
        let order = (*page).order;

        // 原版的健全性检查：order 越界、page->next 未页对齐、魔数不对 → 拒绝
        if order > MAX_ORDER || ((*page).next & (PAGE_SIZE - 1)) != 0 || (*hdr).flags != MF_USED {
            // 原版：printk("kfree of non-kmalloced memory: ...")
            return;
        }

        let tbl = sizes();
        (*hdr).flags = MF_FREE;
        (*hdr).next = (*page).firstfree;
        (*page).firstfree = p;
        (*page).nfree += 1;

        if (*page).nfree == 1 {
            // 从"满"变成"有空位"，挂回档位链表
            (*page).next = tbl[order].firstfree;
            tbl[order].firstfree = page as usize;
        }

        if (*page).nfree == tbl[order].nblocks {
            // 整页空了，从链表摘掉并还给页帧分配器。同原版末段。
            let pa = page as usize;
            if tbl[order].firstfree == pa {
                tbl[order].firstfree = (*page).next;
            } else {
                let mut pg2 = tbl[order].firstfree;
                while pg2 != 0 && (*(pg2 as *mut PageDescriptor)).next != pa {
                    pg2 = (*(pg2 as *mut PageDescriptor)).next;
                }
                if pg2 == 0 {
                    // 原版：printk("Ooops. page doesn't show on freelist.")
                    return;
                }
                (*(pg2 as *mut PageDescriptor)).next = (*page).next;
            }
            tbl[order].npages -= 1;
            free_page(pa);
        }
    }
}

/// 各档位当前占用的页数，自检用。原版对应 `sizes[order].npages`。
pub fn stats() -> [usize; MAX_ORDER + 1] {
    // SAFETY: 只读档位表，早期单线程。
    let tbl = unsafe { sizes() };
    let mut out = [0usize; MAX_ORDER + 1];
    for (i, s) in tbl.iter().enumerate() {
        out[i] = s.npages;
    }
    out
}

/// 关中断的 RAII 守卫。`kmalloc`/`kfree` 里有多个 `return`，用守卫比
/// 在每条返回路径上手写 `restore_flags` 可靠。原版靠 C 的单出口 +
/// `restore_flags(flags)` 达到同样效果。
struct IrqGuard(u64);

impl IrqGuard {
    /// # Safety
    /// 调用者负责在守卫存活期间不睡（睡会带着关中断状态切走）。
    unsafe fn new() -> Self {
        // SAFETY: 契约转交；与 Drop 里的 restore 配对。
        Self(unsafe { crate::irq::local_irq_save() })
    }
}

impl Drop for IrqGuard {
    fn drop(&mut self) {
        // SAFETY: self.0 是 new() 里存下的原始 flags，与之配对恢复。
        unsafe { crate::irq::restore_flags(self.0) }
    }
}
