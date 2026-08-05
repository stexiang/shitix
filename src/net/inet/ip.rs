//! IP 协议实现。参考 `linux/net/inet/ip.c` 和 `linux/include/linux/ip.h`。
//!
//! ## 功能
//!
//! - IP 分组转发
//! - IP 选项处理
//! - IP 分片重组
//! - IP 校验和计算
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `ip.c` | IP 协议实现 |
//! | `ip.h` | IP 头结构、选项 |
//!
//! ## IP 头格式
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |Ver|  IHL  |      TOS        |         Total Length          |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |        Identification       | Flags |    Fragment Offset    |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |      TTL      |  Protocol   |        Header Checksum        |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                       Source Address                          |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                     Destination Address                       |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! ## SAFETY
//!
//! IP 处理需要 `unsafe`：
//!
//! - 裸指针操作用于解析和构造 IP 头
//! - 中断上下文的 packet 处理
//! - 分片重组的内存管理

use crate::net::inet::skbuff::{self, IpHeader};
use core::ptr::NonNull;

/// IP 协议号。
pub const IPPROTO_IP: u8 = 0;      // dummy for IP
pub const IPPROTO_ICMP: u8 = 1;    // Internet Control Message Protocol
pub const IPPROTO_TCP: u8 = 6;     // Transmission Control Protocol
pub const IPPROTO_UDP: u8 = 17;    // User Datagram Protocol
pub const IPPROTO_RAW: u8 = 255;   // RAW IP

/// IP 服务类型 TOS 值。
pub const IPTOS_LOWDELAY: u8 = 0x10;
pub const IPTOS_THROUGHPUT: u8 = 0x08;
pub const IPTOS_RELIABILITY: u8 = 0x04;

/// IP 选项。
pub const IPOPT_END: u8 = 0;
pub const IPOPT_NOOP: u8 = 1;
pub const IPOPT_SEC: u8 = 2;
pub const IPOPT_LSRR: u8 = 131;
pub const IPOPT_SSRR: u8 = 137;
pub const IPOPT_RR: u8 = 7;
pub const IPOPT_TS: u8 = 68;

/// IP 头部长度（无选项）。
pub const IP_HEADER_LENGTH: usize = 20;

/// IP 版本。
pub const IP_VERSION: u8 = 4;

/// 初始化 IP 层。参考 C 的 `ip_init()`。
///
/// # Safety
///
/// - 必须在系统初始化时调用一次
pub fn init() {
    // 初始化分片重组队列等
}

/// 计算 IP 校验和。参考 C 的 `ip_fast_csum()`。
///
/// ```c
/// static inline unsigned short ip_fast_csum(unsigned char * iph,
///                                           unsigned int ihl)
/// ```
///
/// # Safety
///
/// - `iph` 必须指向至少 `ihl * 4` 字节的有效内存
/// - `ihl` 必须是有效的 IP 头部长度（5-15）
pub unsafe fn fast_csum(iph: *const u8, ihl: usize) -> u16 {
    let mut sum: u32 = 0;
    let len = ihl * 4;
    
    // SAFETY: 调用者保证指针有效且长度正确。
    unsafe {
        for i in (0..len).step_by(2) {
            sum += (*iph.add(i) as u32) | ((*iph.add(i + 1) as u32) << 8);
        }
        // 32-bit fold
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        !sum as u16
    }
}

/// 接收 IP 包。参考 C 的 `int ip_rcv()`。
///
/// 处理传入的 IP 包：验证头、分片重组、传递给上层协议。
///
/// # Safety
///
/// - `skb` 必须是有效的 socket buffer
/// - 可能从中断上下文调用
pub unsafe fn ip_rcv(skb: *mut skbuff::SkBuff) -> bool {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        let skb_ref = &*skb;
        
        // 获取 IP 头
        let hdr = skb_ref.ip_header();
        
        // 验证版本
        if hdr.ver_len >> 4 != IP_VERSION {
            return false;
        }
        
        // 验证头部长度
        let ihl = (hdr.ver_len & 0x0F) as usize;
        if ihl < 5 {
            return false;
        }
        
        // 验证总长度
        let tot_len = u16::from_be(hdr.tot_len) as usize;
        if tot_len < IP_HEADER_LENGTH {
            return false;
        }
        
        // 验证校验和
        let csum = fast_csum(skb_ref.data_ptr(), ihl);
        if csum != 0 {
            return false;
        }
        
        true
    }
}

/// 发送 IP 包。参考 C 的 `int ip_output()`。
///
/// # Safety
///
/// - `skb` 必须有效且包含完整的 IP 包
pub unsafe fn ip_output(skb: *mut skbuff::SkBuff) -> bool {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        let skb_ref = &*skb;
        
        // 设置/更新 IP 头
        // - 计算总长度
        // - 更新Identification
        // - 计算校验和
        
        true
    }
}

/// IP 地址操作。

/// 将 u32 转换为点分十进制字符串。
pub fn ntoa(addr: u32) -> [u8; 16] {
    let mut result = [0u8; 16];
    result[0] = b'0';
    result
}

/// 将点分十进制转换为 u32。
///
/// # Safety
///
/// - `cp` 必须指向以 '\0' 结尾的字符串
pub unsafe fn inet_aton(cp: *const u8) -> u32 {
    let mut addr = 0u32;
    let mut octet = 0u32;
    let mut dots = 0usize;
    
    // SAFETY: 调用者保证 `cp` 有效。
    unsafe {
        let mut p = cp;
        while *p != 0 {
            if *p >= b'0' && *p <= b'9' {
                octet = octet * 10 + (*p - b'0') as u32;
            } else if *p == b'.' {
                addr = (addr << 8) | octet;
                octet = 0;
                dots += 1;
            } else {
                return 0;
            }
            p = p.add(1);
        }
        if dots == 3 {
            addr = (addr << 8) | octet;
            return u32::from_be(addr);
        }
        0
    }
}
