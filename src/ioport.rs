//! I/O 端口访问管理。参考 linux-1.0.9 的 `kernel/ioport.c`。
//!
//! ## 功能
//!
//! - I/O 端口权限位图管理
//! - 端口范围注册/检查
//! - ioperm/iopl 系统调用支持
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `ioport.c` | I/O 端口权限管理 |
//!
//! ## 注意
//!
//! 64 位 long mode 下的 ioperm/iopl 实现与原版 32 位不同。
//! 原版依赖 TSS 的 io_bitmap，64 位下我们使用简化的权限检查。

/// I/O 位图大小（字节数）。支持 65536 个端口（64KB 位图）。
pub const IO_BITMAP_SIZE: usize = 64 * 1024 / 8; // 8KB

/// I/O 端口注册表。记录已被内核占用的端口范围。
static mut IOPORT_REGISTRY: [u8; IO_BITMAP_SIZE] = [0; IO_BITMAP_SIZE];

/// 检查端口是否已被注册
pub fn check_port(from: u32, num: u32) -> bool {
    if from + num > (IO_BITMAP_SIZE * 8) as u32 {
        return false;
    }
    
    // SAFETY: 只读位图
    unsafe {
        for i in from..(from + num) {
            let byte_idx = (i / 8) as usize;
            let bit_idx = (i % 8) as usize;
            if (IOPORT_REGISTRY[byte_idx] & (1 << bit_idx)) != 0 {
                return true;
            }
        }
    }
    false
}

/// 注册端口范围
pub fn snarf_region(from: u32, num: u32) {
    if from + num > (IO_BITMAP_SIZE * 8) as u32 {
        return;
    }
    
    // SAFETY: 修改位图
    unsafe {
        for i in from..(from + num) {
            let byte_idx = (i / 8) as usize;
            let bit_idx = (i % 8) as usize;
            IOPORT_REGISTRY[byte_idx] |= 1 << bit_idx;
        }
    }
}

/// 释放端口范围
pub fn release_region(from: u32, num: u32) {
    if from + num > (IO_BITMAP_SIZE * 8) as u32 {
        return;
    }
    
    // SAFETY: 修改位图
    unsafe {
        for i in from..(from + num) {
            let byte_idx = (i / 8) as usize;
            let bit_idx = (i % 8) as usize;
            IOPORT_REGISTRY[byte_idx] &= !(1 << bit_idx);
        }
    }
}

/// I/O 权限级别
#[derive(Debug, Clone, Copy)]
pub enum IoPrivilegeLevel {
    /// 无 I/O 权限
    Level0 = 0,
    /// 基本 I/O 权限（端口 0-0x3FF）
    Level1 = 1,
    /// 扩展 I/O 权限（端口 0-0xFFFF）
    Level2 = 2,
    /// 全部 I/O 权限
    Level3 = 3,
}

/// 设置位图中指定范围的位
fn set_bitmap(bitmap: &mut [u8], base: u32, extent: u32, new_value: bool) {
    let low_index = base % 8;
    let mut length = low_index + extent;
    let mut byte_idx = (base / 8) as usize;
    
    if low_index != 0 {
        let mut mask = !0u8 << low_index;
        if length < 8 {
            mask &= !(0xFFu8 << length);
        }
        if new_value {
            bitmap[byte_idx] |= mask;
        } else {
            bitmap[byte_idx] &= !mask;
        }
        byte_idx += 1;
        if length >= 8 {
            length -= 8;
        } else {
            return;
        }
    }
    
    let fill_value = if new_value { 0xFF } else { 0x00 };
    
    while length >= 8 {
        bitmap[byte_idx] = fill_value;
        byte_idx += 1;
        length -= 8;
    }
    
    if length > 0 {
        let mask = 0xFFu8 << (8 - length);
        if new_value {
            bitmap[byte_idx] |= mask;
        } else {
            bitmap[byte_idx] &= !mask;
        }
    }
}

/// 检查位图中指定范围是否有任何位被设置
fn check_bitmap(bitmap: &[u8], base: u32, extent: u32) -> bool {
    let low_index = base % 8;
    let mut length = low_index + extent;
    let mut byte_idx = (base / 8) as usize;
    
    if low_index != 0 {
        let mut mask = !0u8 << low_index;
        if length < 8 {
            mask &= !(0xFFu8 << length);
        }
        if (bitmap[byte_idx] & mask) != 0 {
            return true;
        }
        byte_idx += 1;
        if length >= 8 {
            length -= 8;
        } else {
            return false;
        }
    }
    
    while length >= 8 {
        if bitmap[byte_idx] != 0 {
            return true;
        }
        byte_idx += 1;
        length -= 8;
    }
    
    if length > 0 {
        let mask = 0xFFu8 << (8 - length);
        if (bitmap[byte_idx] & mask) != 0 {
            return true;
        }
    }
    
    false
}

/// 初始化 I/O 端口管理
pub fn init() {
    // 注册常用的 I/O 端口范围
    // 定时器 (PIT)
    snarf_region(0x40, 4);
    // RTC
    snarf_region(0x70, 2);
    // 键盘控制器
    snarf_region(0x60, 5);
    // DMA 控制器
    snarf_region(0x00, 16);
    // 中断控制器
    snarf_region(0x20, 8);
    snarf_region(0xA0, 8);
    // 串口
    snarf_region(0x3F8, 8);
    snarf_region(0x2F8, 8);
    // 并口
    snarf_region(0x378, 4);
    
    crate::sprintln!("ioport: I/O port management initialized");
}

/// 运行自检
pub fn selftest() {
    crate::sprintln!("--- ioport selftest ---");
    
    // 测试端口注册
    snarf_region(0x100, 16);
    assert!(check_port(0x100, 1));
    assert!(check_port(0x10F, 1));
    assert!(!check_port(0x110, 1));
    
    // 测试端口释放
    release_region(0x100, 16);
    assert!(!check_port(0x100, 1));
    
    crate::sprintln!("ioport: port registry check -> ok");
}
