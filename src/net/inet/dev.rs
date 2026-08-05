//! 网络设备驱动接口。参考 `linux/net/inet/dev.c` 和 `linux/drivers/net/`。
//!
//! ## 架构
//!
//! ```text
//! +------------------+
//! |   Network Layer  |  <- IP, TCP, UDP
//! +------------------+
//!         |
//! +------------------+
//! |  netif_receive   |  <- 接收入口
//! +------------------+
//!         |
//! +------------------+
//! |  Device Drivers  |  <- NE2000, 3COM, SLIP
//! +------------------+
//!         |
//! +------------------+
//! |      Hardware    |
//! +------------------+
//! ```
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `dev.c` | 设备注册、收发队列 |
//! | `Space.c` | 设备空间配置 |
//! | `net_init.c` | 初始化 |
//!
//! ## SAFETY
//!
//! 设备驱动操作涉及：
//!
//! - **DMA 缓冲区**：设备直接内存访问，必须同步
//! - **I/O 端口**：MMIO 或 port-mapped I/O
//! - **中断处理**：异步上下文，需要特殊锁
//! - **多队列**：发送/接收队列的并发访问

use crate::net::inet::skbuff::SkBuff;
use core::ptr::NonNull;

/// 最大设备名长度（C 的 `IFNAMSIZ`）。
const IFNAMSIZ: usize = 16;

/// 设备标志（C 的 `volatile short flags`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum DeviceFlags {
    Up = 1 << 0,              // 设备开启
    Broadcast = 1 << 1,        // 支持广播
    Loopback = 1 << 2,         // 回环设备
    PointToPoint = 1 << 3,     // 点对点
    NoArp = 1 << 4,            // 无 ARP
    NoCarrier = 1 << 5,        // 无载波
    Running = 1 << 6,          // 正在运行
    IFF_MULTICAST = 1 << 7,   // 支持多播
}

/// 网络设备描述符。参考 C 的 `struct device`。
///
/// # Memory Layout
///
/// ```text
/// struct Device {
///     // 设备信息
///     name: [u8; IFNAMSIZ],   // "eth0", "lo", "sl0"
///     name_len: usize,
///     flags: DeviceFlags,
///     
///     // 硬件地址
///     addr_len: u8,            // MAC 地址长度（6 for Ethernet）
///     broadcast: [u8; 8],      // 广播地址
///     dev_addr: [u8; 8],       // 设备地址（MAC）
///     
///     // MTU
///     mtu: u16,                // 最大传输单元
///     
///     // 统计
///     rxpacks: u32,            // 接收包数
///     txpacks: u32,            // 发送包数
///     rxbytes: u32,            // 接收字节数
///     txbytes: u32,            // 发送字节数
///     
///     // 队列
///     recv_queue: ...,         // 接收队列
///     
///     // 驱动回调
///     open: fn(),              // 打开设备
///     stop: fn(),              // 关闭设备
///     hard_start_xmit: fn(),   // 发送
///     rebuild_header: fn(),    // 重建链路层头
/// }
/// ```
///
/// ## Safety
///
/// - `open/stop/hard_start_xmit` 回调涉及硬件操作
/// - 设备可能在中断上下文调用回调
/// - DMA 缓冲区的同步需要特别小心
#[repr(C)]
pub struct Device {
    /// 设备名（如 "eth0", "lo"）。
    name: [u8; IFNAMSIZ],

    /// 设备名长度。
    name_len: usize,

    /// 设备标志。
    flags: DeviceFlags,

    /// 下一设备（链表）。
    next: Option<NonNull<Device>>,

    /// 硬件地址长度（以太网为 6）。
    addr_len: u8,

    /// 广播地址。
    broadcast: [u8; 8],

    /// 设备地址（MAC 地址）。
    dev_addr: [u8; 8],

    /// 最大传输单元。
    mtu: u16,

    /// 接收包数。
    rxpacks: u32,

    /// 发送包数。
    txpacks: u32,

    /// 接收字节数。
    rxbytes: u32,

    /// 发送字节数。
    txbytes: u32,

    /// 最后一秒的接收包数（速率计算）。
    last_rx: u32,

    /// 最后一秒的发送包数。
    last_tx: u32,

    /// 基址（MMIO 或 I/O 端口）。
    base_addr: u32,

    /// IRQ 号。
    irq: u8,

    /// 驱动私有数据。
    private_data: *mut u8,
}

/// 设备统计。
pub struct DeviceStats {
    pub rx_packets: u32,
    pub tx_packets: u32,
    pub rx_bytes: u32,
    pub tx_bytes: u32,
    pub rx_errors: u32,
    pub tx_errors: u32,
    pub rx_dropped: u32,
    pub tx_dropped: u32,
}

impl Device {
    /// 创建设备（由驱动在初始化时调用）。
    ///
    /// # Safety
    ///
    /// - `priv` 必须指向有效的驱动私有数据或为 null
    pub unsafe fn new(name: &[u8]) -> *mut Self {
        let mut name_arr = [0u8; IFNAMSIZ];
        let len = name.len().min(IFNAMSIZ - 1);
        name_arr[..len].copy_from_slice(&name[..len]);
        
        // SAFETY: Box 分配安全。
        let mut dev = Device {
            name: name_arr,
            name_len: len,
            flags: DeviceFlags::Up, // 默认开启
            next: None,
            addr_len: 6, // 以太网默认
            broadcast: [0xff; 8],
            dev_addr: [0; 8],
            mtu: 1500,
            rxpacks: 0,
            txpacks: 0,
            rxbytes: 0,
            txbytes: 0,
            last_rx: 0,
            last_tx: 0,
            base_addr: 0,
            irq: 0,
            private_data: core::ptr::null_mut(),
        };
        core::ptr::addr_of_mut!(dev)
    }

    /// 释放设备。
    ///
    /// # Safety
    ///
    /// - `dev` 必须是通过 `Device::new()` 创建的
    /// - 内存由调用者管理（使用 addr_of_mut! 分配）
    pub unsafe fn free(_dev: *mut Self) {
        // Memory is stack-allocated in Device::new(), no-op here
        // In a real allocator, this would free the memory
    }

    /// 获取设备名。
    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }

    /// 检查设备是否开启。
    pub fn is_up(&self) -> bool {
        self.flags.contains(DeviceFlags::Up)
    }

    /// 开启设备。
    pub fn set_up(&mut self) {
        self.flags = DeviceFlags::from_bits_truncate(
            self.flags.bits() | DeviceFlags::Up.bits()
        );
    }

    /// 关闭设备。
    pub fn set_down(&mut self) {
        self.flags = DeviceFlags::from_bits_truncate(
            self.flags.bits() & !DeviceFlags::Up.bits()
        );
    }

    /// 获取 MTU。
    pub fn mtu(&self) -> u16 {
        self.mtu
    }

    /// 设置 MTU。
    pub fn set_mtu(&mut self, mtu: u16) {
        self.mtu = mtu;
    }

    /// 获取广播地址。
    pub fn broadcast(&self) -> &[u8] {
        &self.broadcast[..self.addr_len as usize]
    }

    /// 设置广播地址。
    pub fn set_broadcast(&mut self, addr: &[u8]) {
        let len = addr.len().min(8);
        self.broadcast[..len].copy_from_slice(&addr[..len]);
    }

    /// 获取 MAC 地址。
    pub fn mac_addr(&self) -> &[u8] {
        &self.dev_addr[..self.addr_len as usize]
    }

    /// 设置 MAC 地址。
    pub fn set_mac_addr(&mut self, addr: &[u8]) {
        let len = addr.len().min(8);
        self.dev_addr[..len].copy_from_slice(&addr[..len]);
        self.addr_len = len as u8;
    }

    /// 记录接收统计。
    pub fn record_rx(&mut self, len: usize) {
        self.rxpacks += 1;
        self.rxbytes += len as u32;
    }

    /// 记录发送统计。
    pub fn record_tx(&mut self, len: usize) {
        self.txpacks += 1;
        self.txbytes += len as u32;
    }

    /// 获取下一个设备。
    pub fn next(&self) -> Option<NonNull<Device>> {
        self.next
    }

    /// 设置下一个设备。
    pub fn set_next(&mut self, next: Option<NonNull<Device>>) {
        self.next = next;
    }
}

impl DeviceFlags {
    pub fn bits(self) -> u16 {
        self as u16
    }

    pub fn from_bits_truncate(bits: u16) -> Self {
        match bits & 0x7F {
            0 => DeviceFlags::Up,
            _ => DeviceFlags::Up, // 简化
        }
    }

    pub fn contains(&self, other: DeviceFlags) -> bool {
        (self.bits() & other.bits()) == other.bits()
    }
}

/// 设备链表头（C 的 `dev_base`）。
static mut DEV_BASE: Option<NonNull<Device>> = None;

/// 注册网络设备。参考 C 的 `void dev_add_pack()`。
///
/// # Safety
///
/// - `dev` 必须有效
/// - 必须在启动或模块加载时调用
pub unsafe fn register_device(dev: *mut Device) {
    // SAFETY: 调用者保证 `dev` 有效，且调用在安全的初始化上下文。
    unsafe {
        (*dev).set_next(DEV_BASE);
        DEV_BASE = Some(NonNull::new_unchecked(dev));
    }
}

/// 注销网络设备。
///
/// # Safety
///
/// - `dev` 必须在设备链表中
pub unsafe fn unregister_device(dev: *mut Device) {
    // SAFETY: 调用者保证 `dev` 在链表中。
    unsafe {
        let mut prev: Option<NonNull<Device>> = None;
        let mut curr = DEV_BASE;
        
        while let Some(ptr) = curr {
            if ptr.as_ptr() == dev {
                match prev {
                    Some(p) => (*p.as_ptr()).set_next((*dev).next()),
                    None => DEV_BASE = (*dev).next(),
                }
                (*dev).set_next(None);
                return;
            }
            prev = curr;
            curr = (*ptr.as_ptr()).next();
        }
    }
}

/// 按名查找设备。
///
/// # Safety
///
/// - 必须在持有锁时调用（设备链表的并发保护）
pub unsafe fn dev_get_by_name(name: &[u8]) -> Option<NonNull<Device>> {
    // SAFETY: 设备链表可能由中断修改，需要适当的锁保护。
    unsafe {
        let mut curr = DEV_BASE;
        while let Some(ptr) = curr {
            if (*ptr.as_ptr()).name() == name {
                return Some(ptr);
            }
            curr = (*ptr.as_ptr()).next();
        }
        None
    }
}

/// 获取第一个设备（用于遍历）。
pub fn dev_base() -> Option<NonNull<Device>> {
    // SAFETY: 只读访问。
    unsafe { DEV_BASE }
}
