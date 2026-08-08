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
/// 已初始化标志
static mut INITIALIZED: bool = false;

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
            if !ide_wait_ready() { continue; }
            ide_select_device(dev, lba);
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
            if !ide_wait_ready() { continue; }
            ide_select_device(dev, lba);
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
            // Delay after select
            for _ in 0..100 { core::hint::spin_loop(); }
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
    if max_lba > 0 && sector + nr_sectors > max_lba {
        crate::pr_warn!("hd: sector {} out of range (max {})\n", sector + nr_sectors, max_lba);
        unsafe { end_request(major_num, false) };
        return;
    }

    let buf = buffer_addr as *mut u8;

    // sector is already in 512-byte LBA units from make_request
    for i in 0..nr_sectors {
        let sector_lba = sector + i;
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
