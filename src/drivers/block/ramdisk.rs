//! 内存盘。对应 linux-1.0.9 的 `drivers/block/ramdisk.c`。
//!
//! 这是我们唯一的块设备，用来当 ROOT_DEV。原版的 ramdisk 是可选的
//! （`CONFIG_BLK_DEV_RAM`），真正的根设备通常是软驱或 IDE 盘；我们不移植
//! `floppy.c`/`hd.c`（各一千多行状态机、DMA、坏道重试），因为它们对
//! 「验证文件系统能跑」这个目标没有增量价值，而 ramdisk 的 `do_rd_request`
//! 只有一次 `memcpy`，能让 I/O 路径的其余部分（请求队列、缓冲缓存、
//! minix 布局）暴露在完全确定的时序下。
//!
//! # 与原版的差异
//!
//! 1. 原版 `rd_init(long mem_start, int length)` 的后备内存来自
//!    `init/main.c` 在 `mem_init` **之前**从物理内存顶部划出的一块
//!    （`memory_end -= ramdisk_size`）。我们的 `mm` 已经初始化完了，
//!    所以直接向页分配器申请连续页——`get_free_page` 只能给单页，因此
//!    这里改成「页表数组 + 按块索引」，见 [`PAGES`]。这也顺带避免了
//!    「必须连续物理内存」这个原版限制。
//! 2. 原版还有 `rd_load()`：从软驱把一个压缩的根映像解压进 ramdisk。
//!    我们没有软驱，映像由 [`format`] 在内存里直接造出来。

use crate::{pr_err, pr_info};
use crate::fs::buffer::NIL;
use crate::fs::{BLOCK_SIZE, READ, WRITE, mkdev};
use crate::mm::page::PAGE_SIZE;
use crate::mm::page_alloc;

use super::ll_rw::{cur, end_request, init_request, register_request_fn};
use super::major::MEM_MAJOR;

/// 次设备号。对应原版 `#define RAMDISK_MINOR 1`。
pub const RAMDISK_MINOR: u32 = 1;

/// ramdisk 的设备号 (1,1)。原版靠 `MKDEV(MEM_MAJOR, RAMDISK_MINOR)`。
pub const RAMDISK_DEV: u16 = mkdev(MEM_MAJOR, RAMDISK_MINOR);

/// 盘容量，单位 [`BLOCK_SIZE`] 块。2048 块 = 2MB。
/// 原版由启动参数 `ramdisk=` 决定（`rd_length`）。
pub const RD_BLOCKS: usize = 2048;

/// 每页装几个块。1024 字节块、4096 字节页 → 4。
const BLOCKS_PER_PAGE: usize = PAGE_SIZE / BLOCK_SIZE;

/// 最大页数。动态分配避免 BSS 压力。
const MAX_PAGES: usize = RD_BLOCKS / BLOCKS_PER_PAGE;

/// 后备存储的页表（动态分配，零 BSS 开销）。
static mut PAGES: *mut usize = core::ptr::null_mut();
/// 当前已分配的页数。
static mut NR_PAGES: usize = 0;

/// 盘是否已建立。原版 `rd_length != 0` 起同样作用。
static mut INITIALIZED: bool = false;

/// 第 `block` 个块在内存里的地址。
///
/// # Safety
/// `block < RD_BLOCKS` 且 [`init`] 已成功执行。
unsafe fn block_addr(block: usize) -> *mut u8 {
    // SAFETY: 契约保证下标在界内、页已分配。
    unsafe {
        let idx = block / BLOCKS_PER_PAGE;
        let page = *PAGES.add(idx);
        // page == 0 说明 init 时这一槽没分到内存（out of memory 那条分支
        // 提前 return 了，但 INITIALIZED 之后仍可能被访问），算出来的地址
        // 会落在低端内存/BIOS ROM 上。读出来是 `0xf000ff53` 那串 IRET
        // stub，被当成块内容灌进缓冲，源头完全看不出来。
        assert!(page >= 0x10_0000, "ramdisk: block {} has no page ({:#x})", block, page);
        (page + (block % BLOCKS_PER_PAGE) * BLOCK_SIZE) as *mut u8
    }
}

/// 处理请求队列。对应原版 `do_rd_request()`。
///
/// 原版那个 `repeat:` / `goto repeat` 循环在这里是 `loop`：一次调用要把
/// 队列里能做的全做完（ramdisk 没有异步完成中断，不做完就没人再来驱动了）。
fn do_rd_request() {
    // SAFETY: 由请求层在持有队列的前提下调用；ramdisk 的 I/O 是同步 memcpy，
    // 不涉及 DMA，也不会睡。
    unsafe {
        loop {
            if !init_request(MEM_MAJOR, "rd") {
                return;
            }
            let (dev, sector, nsec, buf, cmd) = {
                let q = cur(MEM_MAJOR);
                (q.dev.unwrap_or(0), q.sector, q.current_nr_sectors, q.buffer, q.cmd)
            };

            // 原版：`addr = rd_start + (CURRENT->sector << 9)`，
            // 越界或次设备号不对就 end_request(0)
            let byte_off = (sector as usize) * super::SECTOR_SIZE;
            let len = (nsec as usize) * super::SECTOR_SIZE;
            let bad = crate::fs::minor(dev) != RAMDISK_MINOR
                || byte_off + len > RD_BLOCKS * BLOCK_SIZE
                || byte_off % BLOCK_SIZE != 0
                || len != BLOCK_SIZE;
            if bad {
                end_request(MEM_MAJOR, false);
                continue;
            }

            let disk = block_addr(byte_off / BLOCK_SIZE);
            // 传输目标必须是一个真实的缓冲数据页。空指针或低端地址说明
            // 请求层填 `q.buffer` 时出了问题（原版这里是 `CURRENT->buffer`，
            // 由 `make_request`/`end_request` 维护）。不挡的话
            // `copy_nonoverlapping` 会报一条泛化的 UB 消息，看不出是谁填错的。
            assert!(
                !buf.is_null() && (buf as usize) >= 0x10_0000,
                "rd: bad request buffer {:#x} (blk={} cmd={} bh={})",
                buf as usize, byte_off / BLOCK_SIZE, cmd, cur(MEM_MAJOR).bh
            );
            match cmd {
                // SAFETY: 上面已校验 byte_off+len 落在盘内、len 恰为一个块；
                // buf 来自缓冲头的 b_data，长度就是 b_size == BLOCK_SIZE；
                // 两块内存来自不同的页分配，不重叠。
                WRITE => core::ptr::copy_nonoverlapping(buf, disk, len),
                READ => core::ptr::copy_nonoverlapping(disk, buf, len),
                _ => panic!("RAMDISK: unknown RAM disk command!"),
            }
            end_request(MEM_MAJOR, true);
        }
    }
}

/// 建立 ramdisk。对应原版 `rd_init()`。
///
/// # Safety
/// 启动期调用一次，此时页分配器已就绪。
pub unsafe fn init() {
    // SAFETY: 契约保证独占且 mm 可用。
    unsafe {
        // 分配 PAGES 指针数组（get_free_page，不计入 BSS）
        let npages = MAX_PAGES;
        let pages_ptr = crate::mm::get_free_page() as *mut usize;
        if pages_ptr.is_null() {
            pr_err!("RAMDISK: out of memory for page table");
            return;
        }
        // 清零页表页
        core::ptr::write_bytes(pages_ptr as *mut u8, 0, crate::mm::PAGE_SIZE);
        PAGES = pages_ptr;
        NR_PAGES = npages;

        for i in 0..npages {
            let p = page_alloc::get_free_page();
            if p == 0 {
                pr_err!("RAMDISK: out of memory at page {}", i);
                return;
            }
            for j in 0..i {
                if *PAGES.add(j) == p {
                    panic!("RAMDISK: page allocator returned {:#x} twice (slots {} and {})", p, j, i);
                }
            }
            *PAGES.add(i) = p;
        }
        for i in 0..npages {
            let p = *PAGES.add(i);
            if crate::fs::buffer::owns_page(p) {
                panic!("RAMDISK: page {:#x} (slot {}) also used by buffer cache", p, i);
            }
        }
        if !register_request_fn(MEM_MAJOR, do_rd_request) {
            pr_err!("RAMDISK: Unable to get major {}.", MEM_MAJOR);
            return;
        }
        // 原版给 blk_size[MEM_MAJOR] 指向 rd_blocksizes
        super::ll_rw::set_blk_size(MEM_MAJOR, RD_BLOCKS as u32);
        *core::ptr::addr_of_mut!(INITIALIZED) = true;
        pr_info!("RAMDISK: {} KB at major {} minor {}", RD_BLOCKS, MEM_MAJOR, RAMDISK_MINOR);
    }
}

/// 直接写一个块，绕过缓冲缓存。给 [`crate::fs::minix::mkfs`] 造根文件系统用。
///
/// 原版没有这个函数：原版的根映像是 `rd_load()` 从软驱读进来的现成 minix
/// 映像。我们没有软驱也没有外部映像，只能在内存里现造，而造的时候缓冲缓存
/// 还没绑定这个设备，走 `bread` 反而要处理「读一个从没写过的块」。
///
/// # Safety
/// 启动期调用（`mount_root` 之前），此时没有 I/O 在飞、缓存里也还没有本
/// 设备的块，否则会与缓存内容不一致。`block < RD_BLOCKS`。
pub unsafe fn raw_write_block(block: usize, data: &[u8]) {
    if block >= RD_BLOCKS {
        return;
    }
    // SAFETY: 契约保证 init 已跑过、下标在界内、无并发 I/O。
    unsafe {
        if !*core::ptr::addr_of!(INITIALIZED) {
            return;
        }
        let dst = core::slice::from_raw_parts_mut(block_addr(block), BLOCK_SIZE);
        dst.fill(0);
        let n = data.len().min(BLOCK_SIZE);
        dst[..n].copy_from_slice(&data[..n]);
    }
}

/// 直接读一个块，绕过缓冲缓存。与 [`raw_write_block`] 配对，供
/// [`crate::fs::minix::mkfs`] 写完之后自校验用（确认落到内存里的内容
/// 确实是刚写的那份，把「mkfs 写错了」和「读路径读错了」区分开）。
///
/// # Safety
/// 同 [`raw_write_block`]：启动期、无并发 I/O、`block < RD_BLOCKS`。
pub unsafe fn raw_read_block(block: usize, out: &mut [u8]) -> bool {
    if block >= RD_BLOCKS {
        return false;
    }
    // SAFETY: 契约保证 init 已跑过、下标在界内、无并发 I/O。
    unsafe {
        if !*core::ptr::addr_of!(INITIALIZED) {
            return false;
        }
        let src = core::slice::from_raw_parts(block_addr(block) as *const u8, BLOCK_SIZE);
        let n = out.len().min(BLOCK_SIZE);
        out[..n].copy_from_slice(&src[..n]);
        true
    }
}

/// 盘是否可用。
pub fn is_ready() -> bool {
    // SAFETY: 只读一个 bool，启动后不再变。
    unsafe { *core::ptr::addr_of!(INITIALIZED) }
}

/// 消掉 `NIL` 的未使用告警（本模块通过 `init_request` 间接用到它的语义）。
#[allow(dead_code)]
const _NIL_USED: usize = NIL;
