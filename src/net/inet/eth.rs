//! Ethernet 封装。参考 `linux/net/inet/eth.c`。
//!
//! ## 功能
//!
//! - Ethernet 头解析和构造
//! - EtherType 到协议处理器的映射
//! - MAC 地址操作
//!
//! ## Ethernet 帧格式
//!
//! ```text
//! +----------------+----------------+----------------+
//! |   Destination  |     Source     |    EtherType   |
//! |    MAC (6)     |    MAC (6)     |      (2)       |
//! +----------------+----------------+----------------+
//! |                 Payload                          |
//! +-----------------------------------------------+
//! ```
//!
//! ## EtherType 值
//!
//! | EtherType | 协议 |
//! |-----------|------|
//! | 0x0800 | IPv4 |
//! | 0x0806 | ARP |
//! | 0x86DD | IPv6 |
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `eth.c` | Ethernet 处理 |
//! | `eth.h` | Ethernet 头结构 |

use crate::net::inet::skbuff::{SkBuff, EthHeader};

/// Ethernet 类型。
pub const ETH_P_IP: u16 = 0x0800;    // Internet Protocol version 4
pub const ETH_P_ARP: u16 = 0x0806;    // Address Resolution Protocol
pub const ETH_P_IPV6: u16 = 0x86DD;   // Internet Protocol version 6
pub const ETH_P_RARP: u16 = 0x8035;   // Reverse ARP

/// Ethernet 地址长度。
pub const ETH_ALEN: usize = 6;

/// Ethernet 帧最小长度。
pub const ETH_ZLEN: usize = 60;

/// Ethernet 帧最大长度（不含 CRC）。
pub const ETH_DATA_LEN: usize = 1500;

/// Ethernet 帧最大长度（含头）。
pub const ETH_FRAME_LEN: usize = 1514;

/// Ethernet 广播地址。
pub const ETH_BROADCAST: [u8; ETH_ALEN] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// 获取 Ethernet 头。
///
/// # Safety
///
/// - `skb` 必须包含有效的 Ethernet 帧
pub unsafe fn eth_header(skb: *mut SkBuff) -> *mut EthHeader {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        skb.cast::<EthHeader>()
    }
}

/// 判断是否为广播帧。
pub fn is_broadcast(addr: &[u8; ETH_ALEN]) -> bool {
    addr == &ETH_BROADCAST
}

/// 判断是否为多播帧。
pub fn is_multicast(addr: &[u8; ETH_ALEN]) -> bool {
    addr[0] & 0x01 != 0
}

/// 接收 Ethernet 帧。参考 C 的 `eth_type_trans()`。
///
/// 分析 EtherType 并将包分发到对应协议。
///
/// # Safety
///
/// - `skb` 必须包含有效的 Ethernet 帧
pub unsafe fn eth_rcv(skb: *mut SkBuff) -> i32 {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        let hdr = eth_header(skb);
        let ethertype = u16::from_be((*hdr).h_proto);
        
        // 根据 EtherType 分发
        match ethertype {
            ETH_P_IP => {
                // 传递给 IP 层
                crate::net::inet::ip::ip_rcv(skb);
            }
            ETH_P_ARP => {
                // 传递给 ARP 层
                crate::net::inet::arp::arp_rcv(skb);
            }
            _ => {
                // 未知协议
            }
        }
        
        0
    }
}

/// 构建 Ethernet 头。参考 C 的 `eth_build_header()`。
///
/// # Safety
///
/// - `skb` 必须包含足够的空间用于 Ethernet 头
pub unsafe fn eth_build_header(
    skb: *mut SkBuff,
    daddr: &[u8; ETH_ALEN],
    saddr: &[u8; ETH_ALEN],
    proto: u16,
) -> i32 {
    // SAFETY: 调用者保证 `skb` 有足够空间。
    unsafe {
        let hdr = eth_header(skb);
        
        (*hdr).h_dest.copy_from_slice(daddr);
        (*hdr).h_source.copy_from_slice(saddr);
        (*hdr).h_proto = u16::to_be(proto);
        
        ETH_ALEN as i32
    }
}

/// 初始化 Ethernet 层。参考 C 的 `eth_init()`。
pub fn init() {
    // 注册 EtherType 处理器
}
