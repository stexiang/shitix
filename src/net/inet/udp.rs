//! UDP 协议实现。参考 `linux/net/inet/udp.c`。
//!
//! ## 功能
//!
//! - 无连接数据报
//! - 简单的校验和验证
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `udp.c` | UDP 协议实现 |
//! | `udp.h` | UDP 头结构 |
//!
//! ## SAFETY
//!
//! UDP 相对简单，但仍需要 `unsafe` 用于：
//!
//! - 端口哈希表访问
//! - socket buffer 操作

use crate::net::inet::skbuff::{SkBuff, UdpHeader};

/// UDP 初始化。参考 C 的 `udp_init()`。
pub fn init() {
    // 初始化 UDP 哈希表
}

/// 处理输入的 UDP 包。参考 C 的 `udp_rcv()`。
///
/// # Safety
///
/// - `skb` 必须有效
pub unsafe fn udp_rcv(skb: *mut SkBuff) -> i32 {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        let hdr = (*skb).udp_header();
        
        let sport = u16::from_be(hdr.source);
        let dport = u16::from_be(hdr.dest);
        let len = u16::from_be(hdr.len);
        
        // TODO: 查找对应的 socket，复制数据到用户空间
        
        0
    }
}

/// UDP 发送。参考 C 的 `udp_send()`。
///
/// # Safety
///
/// - socket 必须有效
pub unsafe fn udp_send(
    sk: *mut crate::net::inet::sock::Socket,
    data: &[u8],
    daddr: u32,
    dport: u16,
) -> i32 {
    // SAFETY: 调用者保证 `sk` 有效。
    unsafe {
        // TODO: 构建 UDP 头和 IP 封装
        
        0
    }
}

/// 计算 UDP 校验和。参考 C 的 `udp_csum()`。
///
/// # Safety
///
/// - `data` 必须指向至少 `len` 字节
pub unsafe fn udp_csum(data: *const u8, len: usize) -> u16 {
    let mut sum: u32 = 0;
    
    // SAFETY: 调用者保证指针有效。
    unsafe {
        for i in (0..len).step_by(2) {
            sum += (*data.add(i) as u32) | ((*data.add(i + 1) as u32) << 8);
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        !sum as u16
    }
}
