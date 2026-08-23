//! ICMP 协议实现。参考 `linux/net/inet/icmp.c`。
//!
//! ## 功能
//!
//! - 差错报告（目的不可达、TTL 超时等）
//! - 诊断（ping 使用 Echo Request/Reply）
//!
//! ## ICMP 类型
//!
//! | Type | 说明 |
//! |------|------|
//! | 0 | Echo Reply |
//! | 3 | Destination Unreachable |
//! | 4 | Source Quench |
//! | 8 | Echo Request |
//! | 11 | Time Exceeded |
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `icmp.c` | ICMP 协议实现 |

use crate::net::inet::skbuff::{SkBuff, IcmpHeader};

/// ICMP 类型。
pub const ICMP_ECHOREPLY: u8 = 0;
pub const ICMP_DEST_UNREACH: u8 = 3;
pub const ICMP_SOURCE_QUENCH: u8 = 4;
pub const ICMP_REDIRECT: u8 = 5;
pub const ICMP_ECHO: u8 = 8;
pub const ICMP_TIME_EXCEEDED: u8 = 11;
pub const ICMP_PARAMETERPROB: u8 = 12;
pub const ICMP_TIMESTAMP: u8 = 13;
pub const ICMP_TIMESTAMPREPLY: u8 = 14;
pub const ICMP_INFO_REQUEST: u8 = 15;
pub const ICMP_INFO_REPLY: u8 = 16;
pub const ICMP_ADDRESS: u8 = 17;
pub const ICMP_ADDRESSREPLY: u8 = 18;

/// ICMP 目的不可达代码。
pub const ICMP_NET_UNREACH: u8 = 0;
pub const ICMP_HOST_UNREACH: u8 = 1;
pub const ICMP_PROT_UNREACH: u8 = 2;
pub const ICMP_PORT_UNREACH: u8 = 3;
pub const ICMP_FRAG_NEEDED: u8 = 4;
pub const ICMP_SR_FAILED: u8 = 5;
pub const ICMP_NET_UNKNOWN: u8 = 6;
pub const ICMP_HOST_UNKNOWN: u8 = 7;
pub const ICMP_HOST_ISOLATED: u8 = 8;
pub const ICMP_NET_UNR_TOS: u8 = 9;
pub const ICMP_HOST_UNR_TOS: u8 = 10;

/// 初始化 ICMP。参考 C 的 `icmp_init()`。
pub fn init() {
    // 注册 ICMP 协议处理器
}

/// 处理输入的 ICMP 包。参考 C 的 `icmp_rcv()`。
///
/// # Safety
///
/// - `skb` 必须有效
pub unsafe fn icmp_rcv(skb: *mut SkBuff) -> i32 {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        let hdr = (*skb).icmp_header();
        
        match hdr.type_ {
            ICMP_ECHO => {
                // 处理 ping
            }
            ICMP_ECHOREPLY => {
                // 处理 ping 响应
            }
            ICMP_DEST_UNREACH => {
                // 目的不可达
            }
            ICMP_TIME_EXCEEDED => {
                // TTL 超时
            }
            _ => {}
        }
        
        0
    }
}

/// 发送 ICMP 目的地不可达。参考 C 的 `icmp_send()`。
///
/// # Safety
///
/// - `skb` 必须有效
pub unsafe fn icmp_send(skb: *mut SkBuff, type_: u8, code: u8) {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        let ip = (*skb).ip_header();
        let src = u32::from_be(ip.daddr); // 原包的目的地 = 我们（回包源）
        let dst = u32::from_be(ip.saddr); // 回包目的 = 原发送者
        // RFC 792：ICMP 错误载荷 = 8 字节头 + 原 IP 头 + 原数据前 8 字节
        let base = (*skb).data_ptr();
        let off = (ip as *const _ as usize).saturating_sub(base as usize);
        let mut pkt = [0u8; 8 + 20 + 8];
        pkt[0] = type_;
        pkt[1] = code;
        pkt[2..4].copy_from_slice(&[0, 0]); // 校验和占位
        pkt[4..8].copy_from_slice(&[0, 0, 0, 0]); // unused
        let ip_hlen = ((ip.ver_len & 0x0F) as usize) * 4;
        let quote = core::cmp::min(ip_hlen + 8, 20 + 8);
        core::ptr::copy_nonoverlapping(base.add(off), pkt.as_mut_ptr().add(8), quote);
        // ICMP 校验和（一个 HEADER+载荷 的 one-pass）
        let total = 8 + quote;
        let mut sum: u32 = 0;
        for i in (0..total).step_by(2) {
            let hi = pkt[i] as u32;
            let lo = if i + 1 < total { pkt[i + 1] as u32 } else { 0 };
            sum += (hi << 8) | lo;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        let csum = !(sum as u16);
        pkt[2..4].copy_from_slice(&csum.to_be_bytes());
        crate::net::inet::netif::send_ip_packet(dst, 1, &pkt[..total]);
        let _ = src;
    }
}
