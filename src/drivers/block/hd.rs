//! IDE 硬盘驱动。
//!
//! 对应 linux-1.0.9 的 `drivers/block/hd.c`。
//!
//! 驱动 IDE (PATA) 硬盘控制器，通过 ATA PIO 模式读写扇区。
//! 支持最多 2 个硬盘（primary IDE 通道的主/从设备）。
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `hd.c` | IDE 磁盘驱动 |

use crate::fs::{BLOCK_SIZE, READ, WRITE};

use super::ll_rw::{end_request, register_request_fn};
use super::major::HD_MAJOR;
use super::SECTOR_SIZE;

// ---- IDE 端口 ----
/// Primary IDE 控制器基地址
const IDE_PRIMARY_BASE: u16 = 0x1F0;
/// Primary IDE 控制寄存器基地址
const IDE_CONTROL_BASE: u16 = 0x3F6;

/// IDE 寄存器偏移（相对于 IDE_PRIMARY_BASE）
const REG_DATA: u16 = 0x00;     // 数据寄存器（16-bit）
const REG_ERROR: u16 = 0x01;    // 错误 / 特性
const REG_NSECTOR: u16 = 0x02;  // 扇区数
const REG_SECTOR: u16 = 0x03;   // 扇区号（LBA 低 8 位）
const REG_LCYL: u16 = 0x04;     // 柱面低 8 位（LBA 中 8 位）
const REG_HCYL: u16 = 0x05;     // 柱面高 8 位（LBA 高 8 位）
const REG_DRIVE: u16 = 0x06;    // 驱动器/磁头
const REG_STATUS: u16 = 0x07;   // 状态 / 命令
const REG_CMD: u16 = 0x07;      // 命令（写）

/// 状态寄存器位
const STATUS_ERR: u8 = 0x01;    // 错误
const STATUS_DRQ: u8 = 0x08;    // 数据请求（准备好传输）
const STATUS_DRDY: u8 = 0x40;   // 驱动器就绪
const STATUS_BSY: u8 = 0x80;    // 忙

/// ATA 命令
const CMD_READ_SECTORS: u8 = 0x20;      // 读扇区（LBA28）
const CMD_WRITE_SECTORS: u8 = 0x30;     // 写扇区（LBA28）
const CMD_IDENTIFY: u8 = 0xEC;          // 识别设备
const CMD_INIT_DEV_PARAMS: u8 = 0x91;   // 初始化设备参数

/// IRQ 号
const HD_IRQ: u32 = 14;

/// 最大硬盘数
const MAX_HD: usize = 2;

// ---- Bus-master DMA ----
/// PCI IDE 控制器 class/subclass
const IDE_PCI_CLASS: u8 = 0x01;
const IDE_PCI_SUBCLASS: u8 = 0x01;
/// BMIDE 寄存器偏移（相对 bus-master I/O base）
const BM_CMD: u16 = 0x00;
const BM_STATUS: u16 = 0x02;
const BM_PRD_ADDR: u16 = 0x04;
/// BM command 位
const BM_CMD_START: u8 = 0x01;
const BM_CMD_WRITE: u8 = 0x08;  // 0=read from disk, 1=write to disk
/// BM status 位
const BM_STATUS_ACTIVE: u8 = 0x01;
const BM_STATUS_ERROR: u8 = 0x02;
const BM_STATUS_IRQ: u8 = 0x04;

/// PRD（Physical Region Descriptor）表项：8 字节
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct Prd {
    addr: u32,   // 物理地址
    count: u16,  // 字节数（0 = 64KB）
    eot: u16,    // bit15 = end of table
}

/// 最多 8 个 PRD 项（每 PRD 最多 64KB，8 项 = 512KB > 单次最大请求）
const MAX_PRD: usize = 8;
/// bus-master I/O 基地址（0 = 未探测/不可用）
static mut BM_BASE: u16 = 0;
/// PRD 表物理页（4KB，放 512 个 PRD 项绰绰有余）
static mut PRD_PAGE: usize = 0;
/// DMA 数据缓冲区物理页（分配一整页，DMA 要求物理连续）
static mut DMA_PAGE: usize = 0;
/// DMA 可用标志
static mut DMA_OK: bool = false;
/// 最大重试次数
const MAX_ERRORS: usize = 16;

// ---- 设备状态 ----
/// 硬盘是否忙
static mut BUSY: [bool; MAX_HD] = [false; MAX_HD];
/// 当前处理的设备号
static mut CURRENT_DEV: usize = 0;
/// 重置标志
static mut RESET_FLAG: bool = false;
/// 硬盘容量（扇区数）
static mut HD_SIZES: [u64; MAX_HD] = [0; MAX_HD];
/// 每个盘的扇区偏移。用于「合并镜像」：内核镜像占起始 1MB，根文件系统
/// 紧随其后，挂 dev(3,0) 时要把所有读写的 LBA 加上这个偏移（2048 扇区）
/// 才能落到根文件系统而不是内核引导区。偏移 0 = 整盘即文件系统（双盘布局）。
static mut HD_OFFSET: [u64; MAX_HD] = [0; MAX_HD];
/// 已初始化标志
static mut INITIALIZED: bool = false;

/// 设置某个盘的扇区偏移。用于合并镜像：根文件系统不在盘首，而在 1MB 偏移处。
///
/// # Safety
/// 启动期、无并发 I/O 时调用一次。设置后该盘的所有读写都加上此偏移。
pub unsafe fn set_offset(dev: usize, offset: u64) {
    if dev < MAX_HD {
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(HD_OFFSET[dev]), offset);
        }
    }
}

/// 查询某个盘的容量（扇区数）。未探测到的盘返回 0。
pub fn drive_size(dev: usize) -> u64 {
    if dev < MAX_HD {
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(HD_SIZES[dev])) }
    } else {
        0
    }
}

// ---- I/O 辅助函数 ----

/// 读 IDE 状态寄存器。
///
/// # Safety
/// IDE 控制器已初始化。
unsafe fn ide_status() -> u8 {
    // SAFETY: IDE_PRIMARY_BASE 是标准端口
    unsafe {
        x86_64::instructions::port::PortReadOnly::<u8>::new(IDE_PRIMARY_BASE + REG_STATUS).read()
    }
}

/// 等待 IDE 控制器不忙（BSY=0）。
///
/// # Safety
/// IDE 控制器已初始化。
unsafe fn ide_wait_ready() -> bool {
    for _ in 0..500000 {
        let status = unsafe { ide_status() };
        if (status & STATUS_BSY) == 0 {
            return (status & STATUS_DRDY) != 0;
        }
    }
    crate::pr_warn!("hd: timeout waiting for drive ready\n");
    false
}

/// 等待 IDE 数据请求就绪（DRQ=1）。
///
/// # Safety
/// IDE 控制器已初始化。
unsafe fn ide_wait_drq() -> bool {
    for _ in 0..500000 {
        let status = unsafe { ide_status() };
        if (status & STATUS_ERR) != 0 {
            return false;
        }
        if (status & STATUS_DRQ) != 0 {
            return true;
        }
    }
    crate::pr_warn!("hd: timeout waiting for DRQ\n");
    false
}

/// 写 IDE 命令寄存器。
///
/// # Safety
/// IDE 控制器已初始化。
unsafe fn ide_write_cmd(cmd: u8) {
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(IDE_PRIMARY_BASE + REG_CMD).write(cmd);
    }
}

/// 选择 IDE 设备（0=主盘, 1=从盘）。
///
/// # Safety
/// IDE 控制器已初始化。
unsafe fn ide_select_device(dev: usize, lba: u64) {
    let drive_byte = if dev == 0 {
        0xE0u8 // 主盘，LBA 模式
    } else {
        0xF0u8 // 从盘，LBA 模式
    } | ((lba >> 24) & 0x0F) as u8;

    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(IDE_PRIMARY_BASE + REG_DRIVE).write(drive_byte);
    }
}

/// 选盘后让选择生效。ATA 规范要求写完 Drive/Head 寄存器后读 4 次状态
/// （约 400ns）让设备完成切换；不读的话紧接的状态检查可能读到「上一个
/// 选中的设备」的状态——缺从盘时上一个是从盘，状态停在 0xFF/0x00，
/// 后续 wait_ready 永远超时。
///
/// # Safety
/// IDE 控制器已初始化。
unsafe fn ide_settle() {
    unsafe {
        for _ in 0..4 {
            let _ = x86_64::instructions::port::PortReadOnly::<u8>::new(IDE_PRIMARY_BASE + REG_STATUS).read();
        }
    }
}

/// 编程 IDE 扇区计数、LBA 地址。
///
/// # Safety
/// IDE 控制器已初始化。
unsafe fn ide_setup_lba(lba: u64, nsectors: u8) {
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(IDE_PRIMARY_BASE + REG_NSECTOR).write(nsectors);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(IDE_PRIMARY_BASE + REG_SECTOR).write(lba as u8);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(IDE_PRIMARY_BASE + REG_LCYL).write((lba >> 8) as u8);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(IDE_PRIMARY_BASE + REG_HCYL).write((lba >> 16) as u8);
    }
}

/// 从 IDE 数据端口读 256 个 u16（一个扇区 = 512 字节）到缓冲区。
///
/// # Safety
/// IDE 控制器已初始化，DRQ 已就绪，`buf` 指向至少 512 字节的可写内存。
unsafe fn ide_read_sector(buf: *mut u16) {
    let mut data_port = x86_64::instructions::port::PortReadOnly::<u16>::new(IDE_PRIMARY_BASE + REG_DATA);
    for i in 0..256 {
        // SAFETY: I/O 端口读（rep insw 等价）
        let word = unsafe { data_port.read() };
        unsafe {
            core::ptr::write_volatile(buf.add(i), word);
        }
    }
}

/// 从缓冲区写 256 个 u16 到 IDE 数据端口（一个扇区 = 512 字节）。
///
/// # Safety
/// IDE 控制器已初始化，DRQ 已就绪，`buf` 指向至少 512 字节的可读内存。
unsafe fn ide_write_sector(buf: *const u16) {
    let mut data_port = x86_64::instructions::port::PortWriteOnly::<u16>::new(IDE_PRIMARY_BASE + REG_DATA);
    for i in 0..256 {
        let word = unsafe { core::ptr::read_volatile(buf.add(i)) };
        // SAFETY: I/O 端口写
        unsafe { data_port.write(word) };
    }
}

// ---- Bus-master DMA 辅助 ----

/// 探测 PCI IDE 控制器的 bus-master I/O 基地址。
/// 对应原版 `ide_init_pci()`。
#[inline(never)]
fn bmide_probe() -> Option<u16> {
    let devices = crate::pci::pci_enumerate();
    let dev = devices.iter().filter_map(|d| d.as_ref())
        .find(|d| d.class_code == IDE_PCI_CLASS && d.subclass == IDE_PCI_SUBCLASS)?;
    // BAR4 是 bus-master IDE I/O base
    let bar = dev.bars[4]?;
    if !bar.is_io || bar.base == 0 { return None; }
    // 使能 bus mastering（PCI command bit 2）
    let bus = dev.bus; let d = dev.device; let f = dev.function;
    let cmd = crate::pci::pci_read16(bus, d, f, 4);
    crate::pci::pci_write16(bus, d, f, 4, cmd | 0x04);
    Some(bar.base as u16)
}

/// 初始化 DMA：分配 PRD 页和数据页，探测 bus-master 端口。
#[inline(never)]
pub fn bmide_init() {
    let bm = match bmide_probe() {
        Some(b) => b,
        None => return,  // 无 bus-master IDE，静默回退 PIO
    };
    let prd = unsafe { crate::mm::get_free_page() };
    let dma = unsafe { crate::mm::get_free_page() };
    if prd == 0 || dma == 0 {
        return;
    }
    // SAFETY: 内核态独占，页已清零
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(BM_BASE), bm);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(PRD_PAGE), prd);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DMA_PAGE), dma);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DMA_OK), true);
    }
}

/// 填充 PRD 表（单缓冲区，不跨页）
fn bmide_setup_prd(buf_phys: u32, count: u16) {
    let prd_base = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(PRD_PAGE)) };
    // SAFETY: prd_base 是有效内核页
    unsafe {
        let prd = prd_base as *mut Prd;
        (*prd).addr = buf_phys;
        (*prd).count = count;
        (*prd).eot = 0x8000; // end of table
    }
}

/// 启动 bus-master DMA 传输并等待完成。返回 true 表示成功。
///
/// # Safety
/// DMA_OK 已确认，buf_phys 是 DMA_PAGE 的物理地址。
unsafe fn bmide_transfer(is_write: bool, buf_phys: u32, count: u16) -> bool {
    let bm = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(BM_BASE)) };
    let prd_page = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(PRD_PAGE)) };

    // 停掉上次传输
    x86_64::instructions::port::PortWriteOnly::<u8>::new(bm + BM_CMD).write(0);

    // 写 PRD 表地址
    x86_64::instructions::port::PortWriteOnly::<u32>::new(bm + BM_PRD_ADDR)
        .write(prd_page as u32);

    // 清状态（IRQ + Error bits are write-1-to-clear）
    x86_64::instructions::port::PortWriteOnly::<u8>::new(bm + BM_STATUS).write(
        BM_STATUS_IRQ | BM_STATUS_ERROR
    );

    // 启动：direction + start
    let dir = if is_write { BM_CMD_WRITE } else { 0 };
    x86_64::instructions::port::PortWriteOnly::<u8>::new(bm + BM_CMD)
        .write(BM_CMD_START | dir);

    // 等待完成（IRQ 位置位 或 error）
    let mut timeout = 100_000u32;
    loop {
        let st = x86_64::instructions::port::PortReadOnly::<u8>::new(bm + BM_STATUS).read();
        if st & BM_STATUS_ACTIVE == 0 {
            // 传输完成
            return st & BM_STATUS_ERROR == 0;
        }
        timeout -= 1;
        if timeout == 0 {
            crate::pr_warn!("hd: DMA timeout");
            return false;
        }
        core::hint::spin_loop();
    }
}

/// DMA 读扇区到 DMA_PAGE，返回 true 表示成功。
///
/// # Safety
/// DMA_OK 已确认，DMA_PAGE 是有效物理页。
unsafe fn bmide_read_sector(dev: usize, lba: u64, buf: *mut u8) -> bool {
    let dma = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DMA_PAGE)) };
    bmide_setup_prd(dma as u32, 512);

    unsafe {
        ide_select_device(dev, lba);
        ide_settle();
        if !ide_wait_ready() { return false; }
        ide_setup_lba(lba, 1);
        ide_write_cmd(0xC8); // READ DMA
        if !bmide_transfer(false, dma as u32, 512) { return false; }
        core::ptr::copy_nonoverlapping(dma as *const u8, buf, 512);
    }
    true
}

/// DMA 写扇区（从 DMA_PAGE 写到盘）。
///
/// # Safety
/// DMA_OK 已确认，buf 指向至少 512 字节。
unsafe fn bmide_write_sector(dev: usize, lba: u64, buf: *const u8) -> bool {
    let dma = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DMA_PAGE)) };
    unsafe {
        core::ptr::copy_nonoverlapping(buf, dma as *mut u8, 512);
    }
    bmide_setup_prd(dma as u32, 512);

    unsafe {
        ide_select_device(dev, lba);
        ide_settle();
        if !ide_wait_ready() { return false; }
        ide_setup_lba(lba, 1);
        ide_write_cmd(0xCA); // WRITE DMA
        bmide_transfer(true, dma as u32, 512)
    }
}

// ---- 扇区读写 ----

/// 读一个扇区（512 字节）从 IDE 硬盘。
///
/// # Safety
/// `buf` 指向至少 512 字节的可写缓冲区。
/// `dev` 是设备索引 (0=主盘, 1=从盘)。
unsafe fn hd_read_sector(dev: usize, lba: u64, buf: *mut u8) -> bool {
    if dev >= MAX_HD { return false; }
    let size = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(HD_SIZES[dev])) };
    if size == 0 || lba >= size { return false; }

    // Retry up to 3 times — PIO can fail under timing variations
    for _ in 0..3u32 {
        unsafe {
            // DMA 路径（bus-master IDE 已初始化时）
            if core::ptr::read_volatile(core::ptr::addr_of!(DMA_OK)) {
                if bmide_read_sector(dev, lba, buf) { return true; }
                // DMA 失败回退 PIO（不 continue，直接走下面 PIO）
            }

            // PIO 路径
            // 必须先选盘再等就绪：状态寄存器反映的是「当前选中」的设备。
            // 若上一个被选中的盘不存在（如缺从盘时探测过从盘），状态会停在
            // BSY=1 或 DRDY=0，这里先选目标盘、读几次状态让选择生效。
            ide_select_device(dev, lba);
            ide_settle();
            if !ide_wait_ready() { continue; }
            ide_setup_lba(lba, 1);
            ide_write_cmd(CMD_READ_SECTORS);
            if ide_wait_drq() {
                ide_read_sector(buf as *mut u16);
                return true;
            }
        }
    }
    false
}

/// 写一个扇区到 IDE 硬盘。
///
/// # Safety
/// `buf` 指向至少 512 字节的可读缓冲区。
unsafe fn hd_write_sector(dev: usize, lba: u64, buf: *const u8) -> bool {
    if dev >= MAX_HD { return false; }
    let size = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(HD_SIZES[dev])) };
    if size == 0 || lba >= size { return false; }

    for _ in 0..3u32 {
        unsafe {
            // DMA 路径
            if core::ptr::read_volatile(core::ptr::addr_of!(DMA_OK)) {
                if bmide_write_sector(dev, lba, buf) { return true; }
            }

            // PIO 路径
            // 先选盘再等就绪，理由同 hd_read_sector。
            ide_select_device(dev, lba);
            ide_settle();
            if !ide_wait_ready() { continue; }
            ide_setup_lba(lba, 1);
            ide_write_cmd(CMD_WRITE_SECTORS);
            if ide_wait_drq() {
                ide_write_sector(buf as *const u16);
                return true;
            }
        }
    }
    false
}

// ---- 探测 ----

/// 检测 IDE 设备是否存在并读取容量。
/// 通过 ATA IDENTIFY 命令获取设备信息。
///
/// # Safety
/// 启动期在中断禁用时调用。
unsafe fn hd_identify(dev: usize) -> Option<u64> {
    // Retry up to 3 times — PIO probe is timing-sensitive
    for attempt in 0..3u32 {
        unsafe {
            // Reset the drive first (only on first attempt)
            if attempt == 0 {
                // Select device and wait for it to be ready
                x86_64::instructions::port::PortWriteOnly::<u8>::new(IDE_PRIMARY_BASE + REG_DRIVE)
                    .write(if dev == 0 { 0xA0u8 } else { 0xB0u8 });
                // Small delay for device selection
                for _ in 0..1000 { core::hint::spin_loop(); }
            }

            if !ide_wait_ready() { continue; }

            ide_select_device(dev, 0);
            // ATA 规范：写完 Drive/Head 后读几次状态（~400ns）让选盘生效，
            // 否则紧接的状态/数据可能还属于上一个选中的设备。
            ide_settle();
            ide_setup_lba(0, 0);

            ide_write_cmd(CMD_IDENTIFY);

            // Check for floating bus (no device)
            let status = ide_status();
            if status == 0 || status == 0xFF {
                return None; // No device on this port
            }

            // Wait for BSY to clear
            if !ide_wait_ready() { continue; }

            // Small delay — some drives need time after BSY clears
            for _ in 0..1000 { core::hint::spin_loop(); }

            let mut identify_data: [u16; 256] = [0; 256];
            if ide_wait_drq() {
                ide_read_sector(identify_data.as_mut_ptr());
                let lba28 = (identify_data[60] as u64) | ((identify_data[61] as u64) << 16);
                if lba28 > 0 {
                    return Some(lba28);
                }
            }
        }
    }
    None
}

// ---- 请求处理 ----
/// 处理块设备请求——从 IDE 读/写扇区。
///
/// 对应原版 `do_hd_request()`。
fn do_hd_request() {
    let major_num = HD_MAJOR as u32;
    let (cmd, sector, nr_sectors, buffer_addr, bh, dev) = {
        let req = unsafe { super::ll_rw::cur(major_num) };
        // Extract device minor from bh->b_dev
        let d = unsafe { crate::fs::buffer::bh(req.bh).b_dev as u32 };
        let drive = (d & 0xFF) as usize; // minor = drive index
        (req.cmd, req.sector as u64, req.nr_sectors as u64, req.buffer, req.bh, drive)
    };

    // Read HD size for this drive
    let max_lba = if dev < MAX_HD {
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(HD_SIZES[dev])) }
    } else { 0 };
    // 合并镜像偏移：dev(3,0) 的根文件系统在 1MB 偏移处，把文件系统相对
    // LBA 加上偏移得到盘上绝对 LBA。dev(3,1)（双盘布局）偏移为 0。
    let offset = if dev < MAX_HD {
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(HD_OFFSET[dev])) }
    } else { 0 };
    if max_lba > 0 && sector + offset + nr_sectors > max_lba {
        crate::pr_warn!("hd: sector {} out of range (max {})\n", sector + offset + nr_sectors, max_lba);
        unsafe { end_request(major_num, false) };
        return;
    }

    let buf = buffer_addr as *mut u8;

    // sector is already in 512-byte LBA units from make_request
    for i in 0..nr_sectors {
        let sector_lba = sector + offset + i;
        let byte_off = (i * SECTOR_SIZE as u64) as usize;

        let ok = if cmd == READ {
            unsafe { hd_read_sector(dev, sector_lba, buf.add(byte_off)) }
        } else if cmd == WRITE {
            unsafe { hd_write_sector(dev, sector_lba, buf.add(byte_off) as *const u8) }
        } else {
            false
        };

        if !ok {
            crate::pr_warn!("hd: I/O error at sector {} drive {}\n", sector_lba, dev);
            unsafe { end_request(major_num, false) };
            return;
        }
    }

    unsafe { end_request(major_num, true) };
    let _ = bh;
    let _ = major_num;
    let _ = dev;
}

/// 检查 IDE 设备是否就绪。
fn is_ready() -> bool {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(INITIALIZED)) }
}

// ---- 初始化 ----

/// 初始化 IDE 硬盘驱动。
///
/// # Safety
/// 启动期调用一次，在中断启用之前。
pub unsafe fn init() {
    // Skip if already initialized
    if unsafe { core::ptr::read_volatile(core::ptr::addr_of!(INITIALIZED)) } {
        return;
    }
    crate::kprintln!("hd: probing IDE drives...");
    bmide_init();

    // 探测 primary IDE 通道的主盘和从盘
    let mut found = 0usize;

    for dev in 0..MAX_HD {
        // SAFETY: 启动期，IDE 端口标准
        let size = unsafe { hd_identify(dev) };
        if let Some(sectors) = size {
            // SAFETY: 探测到设备后记录容量
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(HD_SIZES[dev]), sectors);
            }
            let size_mb = (sectors * 512) / (1024 * 1024);
            crate::kprintln!("hd: /dev/hd{} - {} MB ({} sectors)", dev as u8 + b'a', size_mb, sectors);
            found += 1;
        }
    }

    if found > 0 {
        // 注册 hd 的请求处理函数
        register_request_fn(HD_MAJOR as u32, do_hd_request);
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(INITIALIZED), true);
        }
        crate::kprintln!("hd: {} drive(s) ready", found);
    } else {
        crate::kprintln!("hd: no IDE drives found");
    }
}
