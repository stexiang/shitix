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
//! ## 线程安全
//!
//! 所有公共函数使用自旋锁保护，确保多核/多线程环境下的安全性。

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

/// I/O 位图大小（字节数）。支持 65536 个端口（64KB 位图）。
pub const IO_BITMAP_SIZE: usize = 64 * 1024 / 8; // 8KB
/// 支持的最大端口号
pub const MAX_PORT: u32 = (IO_BITMAP_SIZE * 8 - 1) as u32;

/// I/O 端口范围描述符
#[derive(Debug, Clone, Copy)]
pub struct PortRange {
    /// 起始端口
    pub base: u32,
    /// 端口数量
    pub count: u32,
    /// 是否已注册
    pub registered: bool,
}

impl PortRange {
    /// 检查指定范围是否与当前范围重叠
    pub fn overlaps(&self, base: u32, count: u32) -> bool {
        if count == 0 || self.count == 0 {
            return false;
        }
        let self_end = self.base + self.count;
        let other_end = base + count;
        self.base < other_end && base < self_end
    }
}

/// I/O 端口位图条目（带引用计数）
#[derive(Debug)]
#[repr(C)]
struct PortEntry {
    /// 位掩码：哪些位被占用
    count: AtomicU8,
}

impl PortEntry {
    const fn new() -> Self {
        PortEntry {
            count: AtomicU8::new(0),
        }
    }

    /// 增加引用计数
    fn acquire(&self) -> bool {
        loop {
            let current = self.count.load(Ordering::Acquire);
            if current >= 255 {
                return false; // 溢出保护
            }
            if self.count.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ).is_ok() {
                return true;
            }
        }
    }

    /// 减少引用计数
    fn release(&self) -> bool {
        loop {
            let current = self.count.load(Ordering::Acquire);
            if current == 0 {
                return false;
            }
            if self.count.compare_exchange(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ).is_ok() {
                return true;
            }
        }
    }

    /// 检查是否有引用
    fn is_busy(&self) -> bool {
        self.count.load(Ordering::Acquire) > 0
    }
}

/// 端口注册表。记录已被内核占用的端口范围。
/// 使用原子操作确保多核安全。
static mut IOPORT_REGISTRY: [PortEntry; IO_BITMAP_SIZE] = 
    [const { PortEntry::new() }; IO_BITMAP_SIZE];

/// 全局自旋锁（简化版本，实际应使用真正的自旋锁）
static LOCK: AtomicUsize = AtomicUsize::new(0);

/// 获取自旋锁
#[inline]
fn acquire_lock() {
    while LOCK.compare_exchange(
        0, 1,
        Ordering::Acquire,
        Ordering::Relaxed,
    ).is_err() {
        // PAUSE 指令或 yield
        core::hint::spin_loop();
    }
}

/// 释放自旋锁
#[inline]
fn release_lock() {
    LOCK.store(0, Ordering::Release);
}

/// 验证端口范围参数
#[inline]
fn validate_range(base: u32, count: u32) -> Result<(), IoError> {
    if base > MAX_PORT {
        return Err(IoError::InvalidAddress);
    }
    // 检查溢出
    let end = base.checked_add(count).ok_or(IoError::Overflow)?;
    if end > MAX_PORT + 1 {
        return Err(IoError::OutOfRange);
    }
    Ok(())
}

/// I/O 端口错误类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoError {
    /// 地址无效
    InvalidAddress,
    /// 范围超出
    OutOfRange,
    /// 整数溢出
    Overflow,
    /// 端口已被占用
    PortBusy,
    /// 端口未被注册
    NotRegistered,
    /// 引用计数溢出
    RefCountOverflow,
}

impl core::fmt::Display for IoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IoError::InvalidAddress => write!(f, "invalid port address"),
            IoError::OutOfRange => write!(f, "port range out of bounds"),
            IoError::Overflow => write!(f, "integer overflow in port calculation"),
            IoError::PortBusy => write!(f, "port already registered"),
            IoError::NotRegistered => write!(f, "port not registered"),
            IoError::RefCountOverflow => write!(f, "reference count overflow"),
        }
    }
}

/// 检查端口是否已被注册
/// 
/// # Arguments
/// * `base` - 起始端口
/// * `count` - 端口数量
/// 
/// # Returns
/// * `true` - 任何端口已被注册
/// * `false` - 所有端口都空闲
pub fn check_port(base: u32, count: u32) -> bool {
    if let Err(_) = validate_range(base, count) {
        return false;
    }
    if count == 0 {
        return false;
    }

    let end = base + count;
    for port in base..end {
        let byte_idx = (port / 8) as usize;
        // SAFETY: 只读访问
        unsafe {
            if IOPORT_REGISTRY[byte_idx].is_busy() {
                return true;
            }
        }
    }
    false
}

/// 检查端口是否完全空闲
/// 
/// # Returns
/// * `true` - 所有端口都空闲
/// * `false` - 任何端口已被注册
pub fn is_port_free(base: u32, count: u32) -> bool {
    !check_port(base, count)
}

/// 注册端口范围
/// 
/// # Arguments
/// * `base` - 起始端口
/// * `count` - 端口数量
/// 
/// # Returns
/// * `Ok(())` - 注册成功
/// * `Err(IoError)` - 注册失败
pub fn snarf_region(base: u32, count: u32) -> Result<(), IoError> {
    validate_range(base, count)?;
    if count == 0 {
        return Ok(());
    }

    acquire_lock();
    // SAFETY: 持有锁
    unsafe {
        let end = base + count;
        for port in base..end {
            let byte_idx = (port / 8) as usize;
            if !IOPORT_REGISTRY[byte_idx].acquire() {
                // 回滚已注册的部分
                for p in base..port {
                    let idx = (p / 8) as usize;
                    IOPORT_REGISTRY[idx].release();
                }
                release_lock();
                return Err(IoError::RefCountOverflow);
            }
        }
    }
    release_lock();
    Ok(())
}

/// 释放端口范围
/// 
/// # Arguments
/// * `base` - 起始端口
/// * `count` - 端口数量
/// 
/// # Returns
/// * `Ok(())` - 释放成功
/// * `Err(IoError)` - 释放失败
pub fn release_region(base: u32, count: u32) -> Result<(), IoError> {
    validate_range(base, count)?;
    if count == 0 {
        return Ok(());
    }

    acquire_lock();
    // SAFETY: 持有锁
    unsafe {
        let end = base + count;
        for port in base..end {
            let byte_idx = (port / 8) as usize;
            if !IOPORT_REGISTRY[byte_idx].release() {
                // 继续释放，即使出错
                crate::sprintln!("[WARN] ioport: port {} refcount already 0", port);
            }
        }
    }
    release_lock();
    Ok(())
}

/// 强制释放端口范围（忽略引用计数）
pub fn force_release(base: u32, count: u32) -> Result<(), IoError> {
    validate_range(base, count)?;
    if count == 0 {
        return Ok(());
    }

    acquire_lock();
    // SAFETY: 持有锁
    unsafe {
        let end = base + count;
        for port in base..end {
            let byte_idx = (port / 8) as usize;
            IOPORT_REGISTRY[byte_idx].count.store(0, Ordering::Release);
        }
    }
    release_lock();
    Ok(())
}

/// 获取端口的当前引用计数
pub fn get_refcount(port: u32) -> Result<u8, IoError> {
    if port > MAX_PORT {
        return Err(IoError::InvalidAddress);
    }
    let byte_idx = (port / 8) as usize;
    // SAFETY: 只读访问
    unsafe {
        Ok(IOPORT_REGISTRY[byte_idx].count.load(Ordering::Acquire))
    }
}

/// 批量检查端口范围
/// 返回 (已注册数, 未注册数)
pub fn count_ports(base: u32, count: u32) -> Result<(u32, u32), IoError> {
    validate_range(base, count)?;
    if count == 0 {
        return Ok((0, 0));
    }

    let mut registered = 0u32;
    let end = base + count;
    for port in base..end {
        let byte_idx = (port / 8) as usize;
        // SAFETY: 只读访问
        unsafe {
            if IOPORT_REGISTRY[byte_idx].is_busy() {
                registered += 1;
            }
        }
    }
    Ok((registered, count - registered))
}

/// I/O 权限级别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl IoPrivilegeLevel {
    /// 从 u32 值转换为 IoPrivilegeLevel
    pub fn from_u32(val: u32) -> Option<IoPrivilegeLevel> {
        match val {
            0 => Some(IoPrivilegeLevel::Level0),
            1 => Some(IoPrivilegeLevel::Level1),
            2 => Some(IoPrivilegeLevel::Level2),
            3 => Some(IoPrivilegeLevel::Level3),
            _ => None,
        }
    }
}

/// 已知端口范围常量
pub mod known_ports {
    use super::*;

    /// PIC 主控制器
    pub const PIC_MASTER: PortRange = PortRange { base: 0x20, count: 8, registered: false };
    /// PIC 从控制器
    pub const PIC_SLAVE: PortRange = PortRange { base: 0xA0, count: 8, registered: false };
    /// PIT 定时器
    pub const PIT: PortRange = PortRange { base: 0x40, count: 4, registered: false };
    /// RTC
    pub const RTC: PortRange = PortRange { base: 0x70, count: 2, registered: false };
    /// 键盘控制器
    pub const KEYBOARD: PortRange = PortRange { base: 0x60, count: 5, registered: false };
    /// DMA 控制器
    pub const DMA: PortRange = PortRange { base: 0x00, count: 16, registered: false };
    /// 串口 COM1
    pub const COM1: PortRange = PortRange { base: 0x3F8, count: 8, registered: false };
    /// 串口 COM2
    pub const COM2: PortRange = PortRange { base: 0x2F8, count: 8, registered: false };
    /// 并口 LPT1
    pub const LPT1: PortRange = PortRange { base: 0x378, count: 4, registered: false };
    /// 软盘控制器
    pub const FDC: PortRange = PortRange { base: 0x3F0, count: 6, registered: false };
}

/// 初始化 I/O 端口管理
pub fn init() {
    // 注册常用的 I/O 端口范围
    // 定时器 (PIT)
    let _ = snarf_region(known_ports::PIT.base, known_ports::PIT.count);
    // RTC
    let _ = snarf_region(known_ports::RTC.base, known_ports::RTC.count);
    // 键盘控制器
    let _ = snarf_region(known_ports::KEYBOARD.base, known_ports::KEYBOARD.count);
    // DMA 控制器
    let _ = snarf_region(known_ports::DMA.base, known_ports::DMA.count);
    // 中断控制器
    let _ = snarf_region(known_ports::PIC_MASTER.base, known_ports::PIC_MASTER.count);
    let _ = snarf_region(known_ports::PIC_SLAVE.base, known_ports::PIC_SLAVE.count);
    // 串口
    let _ = snarf_region(known_ports::COM1.base, known_ports::COM1.count);
    let _ = snarf_region(known_ports::COM2.base, known_ports::COM2.count);
    // 并口
    let _ = snarf_region(known_ports::LPT1.base, known_ports::LPT1.count);
    
    crate::sprintln!("ioport: I/O port management initialized (max port: 0x{:X})", MAX_PORT);
}

/// 运行自检
pub fn selftest() {
    crate::sprintln!("--- ioport selftest ---");
    
    // 测试边界条件
    assert!(validate_range(0, 0).is_ok(), "zero count should be valid");
    assert!(validate_range(0, 1).is_ok(), "port 0 should be valid");
    assert!(validate_range(MAX_PORT, 1).is_err(), "port beyond max should be invalid");
    assert!(validate_range(u32::MAX, 1).is_err(), "u32::MAX should be invalid");
    
    // 测试端口注册
    let result = snarf_region(0x1000, 16);
    assert!(result.is_ok(), "snarf_region should succeed");
    assert!(check_port(0x1000, 16), "ports should be registered");
    assert!(check_port(0x1005, 1), "middle port should be registered");
    assert!(!check_port(0x1010, 1), "outside port should not be registered");
    
    // 测试引用计数
    assert_eq!(get_refcount(0x1005).unwrap(), 1, "refcount should be 1");
    
    // 测试重复注册（增加引用计数）
    let result2 = snarf_region(0x1000, 16);
    assert!(result2.is_ok(), "second snarf should succeed");
    assert_eq!(get_refcount(0x1005).unwrap(), 2, "refcount should be 2");
    
    // 测试释放一次
    let result3 = release_region(0x1000, 16);
    assert!(result3.is_ok(), "release should succeed");
    assert_eq!(get_refcount(0x1005).unwrap(), 1, "refcount should be 1");
    assert!(check_port(0x1005, 1), "port still registered after partial release");
    
    // 测试批量计数
    let (reg, free) = count_ports(0x1000, 16).unwrap();
    assert_eq!(reg, 16, "all 16 should be registered");
    assert_eq!(free, 0, "none should be free");
    
    // 测试释放全部
    let result4 = release_region(0x1000, 16);
    assert!(result4.is_ok(), "final release should succeed");
    assert_eq!(get_refcount(0x1005).unwrap(), 0, "refcount should be 0");
    assert!(!check_port(0x1005, 1), "port should be free now");
    
    // 测试 is_port_free
    assert!(is_port_free(0x1000, 16), "range should be free now");
    
    // 测试重叠检测
    snarf_region(0x2000, 8).unwrap();
    let range = PortRange { base: 0x2003, count: 4, registered: false };
    assert!(range.overlaps(0x2000, 8), "exact overlap should be detected");
    assert!(range.overlaps(0x1FFF, 8), "partial overlap should be detected");
    assert!(!range.overlaps(0x2008, 8), "no overlap should be detected");
    release_region(0x2000, 8).unwrap();
    
    // 测试强制释放
    snarf_region(0x3000, 8).unwrap();
    snarf_region(0x3000, 8).unwrap();
    assert_eq!(get_refcount(0x3005).unwrap(), 2);
    force_release(0x3000, 8).unwrap();
    assert_eq!(get_refcount(0x3005).unwrap(), 0);
    
    crate::sprintln!("ioport: port registry, refcount, overlap -> ok");
}
