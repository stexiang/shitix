//! Socket Buffer（sk_buff）。参考 `linux/net/inet/skbuff.h`。
//!
//! `struct sk_buff` 是网络数据包的内核表示。每个包一个 `SkBuff`，
//! 包含包的元数据（长度、协议、源/目的地址）和指向实际数据的指针。
//!
//! ## 关键字段（C 对照）
//!
//! | C 字段 | Rust 字段 | 说明 |
//! |--------|-----------|------|
//! | `next`, `prev` | `list_link` | 链表链接（用于队列）|
//! | `sk` | `socket` | 所属 socket |
//! | `dev` | `device` | 发送/接收的网络设备 |
//! | `len` | `len` | 包总长度 |
//! | `data` | `data` | 指向包数据的指针 |
//! | `h.th/h.iph/h.uh` | `hdr` | 协议头指针（TCP/IP/UDP）|
//!
//! ## Memory Layout
//!
//! ```text
//! +------------------+
//! |   SkBuff head    |  <- 元数据（固定大小）
//! +------------------+
//! |   Protocol hdr   |  <- TCP/UDP/IP/ETH header pointers
//! +------------------+
//! |   Packet data    |  <- 可变长数据区
//! +------------------+
//! ```
//!
//! ## SAFETY
//!
//! `SkBuff` 操作涉及裸指针和链表修改，需要 `unsafe`：
//!
//! - 链表操作：在持有独占访问权时修改是安全的
//! - DMA 缓冲区：必须确保数据在设备 DMA 范围内
//! - 中断上下文：使用 `spin_lock_irqsave` 保护链表

use core::ptr::NonNull;

/// Magic numbers for debugging。参考 `linux/net/inet/skbuff.h`。
const SK_FREED_SKB: u32 = 0x0DE2C0DE;
const SK_GOOD_SKB: u32 = 0xDEC0DED1;

/// Socket buffer。参考 C 的 `struct sk_buff`。
///
/// # Memory Layout
///
/// ```text
/// struct SkBuff {
///     // 链表管理（4 words = 32 bytes on 64-bit）
///     list_link: Option<NonNull<SkBuff>>,  // 队列中的下一个
///     
///     // Socket 关联
///     socket: Option<NonNull<Socket>>,     // 所属 socket
///     
///     // 设备
///     device: Option<NonNull<Device>>,     // 网络设备
///     
///     // 包信息
///     len: u32,                            // 总长度
///     data_len: u32,                       // 数据区长度
///     data: NonNull<u8>,                   // 包数据指针
///     
///     // 协议层指针（C 的 union h{}）
///     hdr: ProtocolHeaders,
///     
///     // 元数据
///     protocol: u16,                       // 上层协议
///     users: u16,                          // 引用计数
///     
///     // 时间戳
///     when: u64,                           // 用于 RTT 计算
///     
///     // 地址
///     saddr: u32,                          // 源地址 (IP)
///     daddr: u32,                          // 目的地址 (IP)
///     
///     // 状态标志
///     flags: SkBuffFlags,
/// }
/// ```
///
/// ## Safety
///
/// - `data` 必须指向有效的、至少 `len` 字节的内存
/// - `list_link` 为 `Some` 时，指向的 `SkBuff` 必须是有效的
/// - `socket` 为 `Some` 时，指向的 `Socket` 必须是有效的
#[repr(C)]
pub struct SkBuff {
    /// 队列链接（用于 `skb_queue_head/tail/dequeue`）。
    /// 在 64 位系统上，这比 C 的 `next/prev` 更简单——只存单向链表。
    list_link: Option<NonNull<SkBuff>>,

    /// 所属 socket（下标，NIL = usize::MAX 表示无）。
    socket: usize,

    /// 网络设备（接收或发送）的下标。
    device: usize,

    /// 包总长度（字节）。
    len: u32,

    /// 数据区长度（不含 sk_buff 头）。
    data_len: u32,

    /// 包数据指针。指向一个 `len` 字节的缓冲区。
    data: NonNull<u8>,

    /// 协议头指针联合体。参考 C 的 `union h{}`。
    hdr: ProtocolHeaders,

    /// 上层协议号（如 `IPPROTO_TCP`）。
    protocol: u16,

    /// 引用计数（C 的 `users`）。防止在多方使用时释放。
    users: u16,

    /// 时间戳（C 的 `when`），用于 RTT 计算。
    when: u64,

    /// 源 IP 地址（C 的 `saddr`）。
    saddr: u32,

    /// 目的 IP 地址（C 的 `daddr`）。
    daddr: u32,

    /// 状态标志。
    flags: SkBuffFlags,

    /// Magic number for debugging（C 的 `magic_debug_cookie`）。
    magic: u32,
}

/// 协议头指针联合体。参考 C 的 `union h{}`。
/// 每个字段指向 `data` 中的特定偏移。
/// 注意：类型定义在本文件末尾。
#[repr(C)]
union ProtocolHeaders {
    /// TCP 头。
    tcp: *mut TcpHeader,
    /// UDP 头。
    udp: *mut UdpHeader,
    /// IP 头。
    ip: *mut IpHeader,
    /// ICMP 头。
    icmp: *mut IcmpHeader,
    /// Ethernet 头。
    eth: *mut EthHeader,
    /// 原始数据。
    raw: *mut u8,
}

/// SkBuff 状态标志。参考 C 的 volatile char 字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SkBuffFlags {
    None = 0,
    /// 包已被确认。
    Acked = 1 << 0,
    /// 包已被使用。
    Used = 1 << 1,
    /// 包需释放（C 的 `free`）。
    Free = 1 << 2,
    /// ARP 已完成。
    ArpDone = 1 << 3,
}

/// TCP 头。参考 `linux/include/linux/tcp.h` 和 C 的 `struct tcphdr`。
#[repr(C, packed)]
pub struct TcpHeader {
    pub source: u16,    // 源端口
    pub dest: u16,      // 目的端口
    pub seq: u32,       // 序列号
    pub ack_seq: u32,   // 确认号
    pub doff_flags: u16, // 数据偏移 + 标志
    pub window: u16,    // 窗口大小
    pub check: u16,     // 校验和
    pub urgent: u16,    // 紧急指针
    // 选项在 TCP 头之后（可变长）
}

/// UDP 头。参考 `linux/include/linux/udp.h`。
#[repr(C, packed)]
pub struct UdpHeader {
    pub source: u16,    // 源端口
    pub dest: u16,      // 目的端口
    pub len: u16,       // UDP 长度
    pub check: u16,     // 校验和
}

/// ICMP 头。参考 `linux/include/linux/ip.h`。
#[repr(C, packed)]
pub struct IcmpHeader {
    pub type_: u8,      // 类型
    pub code: u8,       // 代码
    pub checksum: u16,  // 校验和
    // 余下 4 字节因类型而异
}

/// IP 头。参考 `linux/include/linux/ip.h`。
#[repr(C, packed)]
pub struct IpHeader {
    pub ver_len: u8,        // 版本 + 头长度
    pub tos: u8,            // 服务类型
    pub tot_len: u16,       // 总长度
    pub id: u16,            // 标识
    pub frag_off: u16,      // 分片偏移 + 标志
    pub ttl: u8,            // 生存时间
    pub proto: u8,          // 协议
    pub check: u16,         // 头校验和
    pub saddr: u32,         // 源地址
    pub daddr: u32,         // 目的地址
    // 选项（可变长，ver_len & 0x0F 决定头部长）
}

/// Ethernet 头。参考 C 的 `struct ethhdr`。
#[repr(C, packed)]
pub struct EthHeader {
    pub h_dest: [u8; 6],    // 目的 MAC
    pub h_source: [u8; 6],   // 源 MAC
    pub h_proto: u16,        // 上层协议
}

impl SkBuff {
    /// 创建新的 SkBuff。参考 `linux/net/inet/skbuff.c` 的 `alloc_skb()`。
    ///
    /// # Safety
    ///
    /// - `data` 必须指向有效的、至少 `size` 字节的内存
    /// - 调用者必须负责在不再需要时释放
    pub unsafe fn new(data: NonNull<u8>, size: usize) -> Self {
        SkBuff {
            list_link: None,
            socket: usize::MAX, // NIL
            device: usize::MAX, // NIL
            len: size as u32,
            data_len: size as u32,
            data,
            hdr: ProtocolHeaders { raw: core::ptr::null_mut() },
            protocol: 0,
            users: 1, // 初始引用
            when: 0,
            saddr: 0,
            daddr: 0,
            flags: SkBuffFlags::None,
            magic: SK_GOOD_SKB,
        }
    }

    /// 增加引用计数。参考 C 的 `atomic_inc(&skb->users)`。
    ///
    /// # Safety
    ///
    /// - `self` 必须有效且 `users > 0`
    /// - 必须在持有锁或确保无竞争时调用
    pub unsafe fn add_users(&mut self) {
        // SAFETY: 调用者保证 `self` 有效且 `users > 0`。单核内核
        // 中没有真正的竞争，但保留原子操作以备未来多核支持。
        self.users = self.users.wrapping_add(1);
    }

    /// 释放引用计数，当达到零时返回 `true`。
    ///
    /// # Safety
    ///
    /// - 调用后若返回 `true`，`self` 不应再被使用
    pub fn release_users(&mut self) -> bool {
        self.users = self.users.wrapping_sub(1);
        self.users == 0
    }

    /// 获取协议头（C 的 `h.th/h.iph/h.uh`）。
    ///
    /// # Safety
    ///
    /// - 指针有效且对齐
    /// - 调用者知道确切的协议类型
    pub unsafe fn tcp_header(&self) -> &TcpHeader {
        // SAFETY: 调用者保证 `hdr.tcp` 有效。
        unsafe { &*self.hdr.tcp }
    }

    /// 获取 UDP 头。
    ///
    /// # Safety
    ///
    /// - 指针有效且对齐
    pub unsafe fn udp_header(&self) -> &UdpHeader {
        // SAFETY: 调用者保证 `hdr.udp` 有效。
        unsafe { &*self.hdr.udp }
    }

    /// 获取 ICMP 头。
    ///
    /// # Safety
    ///
    /// - 指针有效且对齐
    pub unsafe fn icmp_header(&self) -> &IcmpHeader {
        // SAFETY: 调用者保证 `hdr.icmp` 有效。
        unsafe { &*self.hdr.icmp }
    }

    /// 获取 IP 头。
    ///
    /// # Safety
    ///
    /// - 指针有效且对齐
    pub unsafe fn ip_header(&self) -> &IpHeader {
        // SAFETY: 调用者保证 `hdr.ip` 有效。
        unsafe { &*self.hdr.ip }
    }

    /// 检查 magic number 是否正确（调试用）。
    pub fn check_magic(&self) -> bool {
        self.magic == SK_GOOD_SKB
    }

    /// 获取数据指针。
    pub fn data_ptr(&self) -> *mut u8 {
        self.data.as_ptr()
    }

    /// 获取数据长度。
    pub fn data_len(&self) -> usize {
        self.data_len as usize
    }

    /// 获取总长度。
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// 调整数据指针（C 的 `skb_pull`）。
    /// 从数据区头部移除 `len` 字节。
    ///
    /// # Safety
    ///
    /// - `len <= self.data_len`
    /// - 调整后必须保持 `data` 有效
    pub unsafe fn pull(&mut self, len: usize) {
        // SAFETY: 调用者保证 `len` 在范围内，且 `data` 有效。
        if len <= self.data_len as usize {
            // SAFETY: 已校验边界。
            unsafe {
                self.data = NonNull::new_unchecked(self.data.as_ptr().add(len));
            }
            self.data_len -= len as u32;
            self.len -= len as u32;
        }
    }
}

/// Socket buffer 队列（C 的 `struct sk_buff *volatile`）。
/// 用于实现包的 FIFO 队列。
pub struct SkBuffQueue {
    /// 队列头。
    head: Option<NonNull<SkBuff>>,
    /// 队列尾。
    tail: Option<NonNull<SkBuff>>,
    /// 队列长度。
    len: usize,
}

impl SkBuffQueue {
    /// 创建空队列。
    pub const fn new() -> Self {
        SkBuffQueue {
            head: None,
            tail: None,
            len: 0,
        }
    }

    /// 入队（C 的 `skb_queue_tail`）。
    ///
    /// # Safety
    ///
    /// - `skb` 必须有效且未在任何其他队列中
    /// - 调用者持有必要的锁
    pub unsafe fn queue_tail(&mut self, skb: *mut SkBuff) {
        // SAFETY: 调用者保证 `skb` 有效。
        unsafe {
            let skb = NonNull::new_unchecked(skb);
            (*skb.as_ptr()).list_link = None;
            
            if let Some(tail) = self.tail {
                (*tail.as_ptr()).list_link = Some(skb);
            } else {
                self.head = Some(skb);
            }
            self.tail = Some(skb);
            self.len += 1;
        }
    }

    /// 出队（C 的 `skb_dequeue`）。
    ///
    /// # Safety
    ///
    /// - 队列非空
    /// - 返回的 `SkBuff` 有效，调用者负责释放
    pub unsafe fn dequeue(&mut self) -> *mut SkBuff {
        if let Some(head) = self.head {
            // SAFETY: 已在上面检查 `self.head` 非空。
            unsafe {
                self.head = (*head.as_ptr()).list_link;
                if self.head.is_none() {
                    self.tail = None;
                }
                self.len -= 1;
                (*head.as_ptr()).list_link = None;
                head.as_ptr()
            }
        } else {
            core::ptr::null_mut()
        }
    }

    /// 获取队列长度。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 检查队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }
}

// 协议号常量（C 的 `IPPROTO_*`）
pub mod ip {
    pub const IPPROTO_IP: u8 = 0;      // dummy for IP
    pub const IPPROTO_ICMP: u8 = 1;    // Internet Control Message Protocol
    pub const IPPROTO_TCP: u8 = 6;     // Transmission Control Protocol
    pub const IPPROTO_UDP: u8 = 17;    // User Datagram Protocol
    pub const IPPROTO_RAW: u8 = 255;   // RAW IP
}

// TCP 标志（C 的 `linux/tcp.h`）
pub mod tcp_flags {
    pub const TCP_FLAG_FIN: u8 = 0x01;
    pub const TCP_FLAG_SYN: u8 = 0x02;
    pub const TCP_FLAG_RST: u8 = 0x04;
    pub const TCP_FLAG_PSH: u8 = 0x08;
    pub const TCP_FLAG_ACK: u8 = 0x10;
    pub const TCP_FLAG_URG: u8 = 0x20;
    pub const TCP_FLAG_ECE: u8 = 0x40;
    pub const TCP_FLAG_CWR: u8 = 0x80;
}
