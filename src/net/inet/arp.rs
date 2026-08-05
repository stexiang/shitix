//! ARP 协议实现。参考 `linux/net/inet/arp.c`。
//!
//! ## 功能
//!
//! - IP 地址到 MAC 地址解析
//! - ARP 缓存管理
//! - 代理 ARP 支持
//!
//! ## ARP 包格式
//!
//! ```text
//! +---------+---------+---------+---------+
//! | HRD | PRO | HLN | PLN | OP  |
//! +---------+---------+---------+---------+
//! | SHA (源 MAC)                        |
//! +---------+---------+---------+---------+
//! | SPA (源 IP)                         |
//! +---------+---------+---------+---------+
//! | THA (目的 MAC)                      |
//! +---------+---------+---------+---------+
//! | TPA (目的 IP)                       |
//! +---------+---------+---------+---------+
//! ```
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `arp.c` | ARP 协议实现 |
//! | `arp.h` | ARP 结构和常量 |

/// ARP 操作码。
pub const ARPOP_REQUEST: u16 = 1;    // ARP 请求
pub const ARPOP_REPLY: u16 = 2;      // ARP 响应
pub const ARPOP_RREQUEST: u16 = 3;   // RARP 请求
pub const ARPOP_RREPLY: u16 = 4;      // RARP 响应

/// ARP 硬件类型。
pub const ARPHRD_ETHER: u16 = 1;     // 以太网

/// ARP 协议类型（与 EtherType 相同）。
pub const ETH_P_ARP: u16 = 0x0806;
pub const ETH_P_IP: u16 = 0x0800;

/// ARP 缓存条目。
#[derive(Debug, Clone, Copy)]
pub struct ArpEntry {
    /// IP 地址。
    ip: u32,
    /// MAC 地址。
    mac: [u8; 6],
    /// 状态。
    state: ArpState,
    /// 过期时间。
    expires: u64,
}

/// ARP 缓存状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArpState {
    /// 空闲。
    Free,
    /// 正在解析。
    Pending,
    /// 已解析。
    Permanent,
    /// 已解析但会过期。
    Stale,
}

/// ARP 缓存表大小。
const ARP_TABLE_SIZE: usize = 64;

/// ARP 缓存表。
pub struct ArpTable {
    entries: [Option<ArpEntry>; ARP_TABLE_SIZE],
}

impl ArpTable {
    /// 创建新的 ARP 表。
    pub const fn new() -> Self {
        ArpTable {
            entries: [None; ARP_TABLE_SIZE],
        }
    }

    /// 计算哈希值。
    fn hash(ip: u32) -> usize {
        ((ip ^ (ip >> 16)) as usize) & (ARP_TABLE_SIZE - 1)
    }

    /// 查找 ARP 条目。
    pub fn lookup(&self, ip: u32) -> Option<&ArpEntry> {
        let h = Self::hash(ip);
        self.entries[h].as_ref()
    }

    /// 添加 ARP 条目。
    ///
    /// # Safety
    ///
    /// - 表可能被多个上下文访问，需要锁
    pub unsafe fn insert(&mut self, ip: u32, mac: &[u8; 6]) {
        let h = Self::hash(ip);
        // SAFETY: 调用者保证表被正确锁定。
        unsafe {
            self.entries[h] = Some(ArpEntry {
                ip,
                mac: *mac,
                state: ArpState::Permanent,
                expires: 0,
            });
        }
    }
}

/// 初始化 ARP。参考 C 的 `arp_init()`。
pub fn init() {
    // 初始化 ARP 表，注册协议
}

/// 处理 ARP 包。参考 C 的 `arp_rcv()`。
///
/// # Safety
///
/// - `skb` 必须有效
pub unsafe fn arp_rcv(skb: *mut crate::net::inet::skbuff::SkBuff) -> i32 {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        // TODO: 解析 ARP 包
        // - 提取操作码 (REQUEST/REPLY)
        // - 提取源/目的 IP 和 MAC
        // - REQUEST: 发送响应
        // - REPLY: 更新 ARP 缓存
        0
    }
}

/// 发送 ARP 请求。参考 C 的 `arp_send()`。
///
/// # Safety
///
/// - `dev` 必须有效
pub unsafe fn arp_send_query(dev: *mut crate::net::inet::dev::Device, target_ip: u32) {
    // SAFETY: 调用者保证 `dev` 有效。
    unsafe {
        // TODO: 构建并发送 ARP 请求
    }
}
