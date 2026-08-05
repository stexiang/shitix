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
        // TODO: 构建并发送 ICMP 错误消息
    }
}
