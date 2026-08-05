//! 协议注册和分发。参考 `linux/net/inet/protocol.c`。
//!
//! ## 功能
//!
//! - 注册网络层协议处理器
//! - 协议号到处理器的映射
//! - IP 层的协议查找
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `protocol.c` | 协议注册 |
//! | `protocol.h` | 协议结构 |

use crate::net::inet::skbuff::SkBuff;

/// 协议处理函数类型。
pub type ProtocolHandler = unsafe fn(skb: *mut SkBuff) -> i32;

/// 协议描述符。参考 C 的 `struct inet_protocol`。
#[derive(Debug)]
pub struct InetProtocol {
    /// 协议名。
    name: &'static str,
    /// 处理函数。
    handler: ProtocolHandler,
    /// 下一协议（链表）。
    next: Option<*mut InetProtocol>,
    /// 协议号。
    protocol: u8,
    /// 标志。
    flags: u8,
}

/// 协议表大小。
const INET_PROTO_HASH_SIZE: usize = 32;

/// 全局协议处理函数数组（C 的 `inet_protos[MAX_INET_PROTOS]`）。
static mut INET_PROTOCOLS: [Option<ProtocolHandler>; 256] = [None; 256];

/// 全局协议描述符链表。
static mut INET_PROTOCOL_BASE: Option<*mut InetProtocol> = None;

/// 注册协议处理器。
///
/// # Safety
///
/// - 必须在系统初始化时调用
/// - `protocol` 必须是有效的协议号
pub fn inet_add_protocol(
    _protocol: u8,
    _handler: ProtocolHandler,
    _name: &'static str,
) -> i32 {
    // TODO: 实现协议注册表
    // SAFETY: 调用在初始化上下文中，无竞争。
    0
}

/// 移除协议处理器。
///
/// # Safety
///
/// - 必须在系统关闭时调用
pub fn inet_del_protocol(_protocol: u8) -> i32 {
    // TODO: 实现协议注销
    // SAFETY: 调用在初始化上下文中，无竞争。
    0
}

/// 根据协议号查找处理器。
///
/// # Safety
///
/// - 调用者保证 `skb` 有效
pub fn inet_protocol(
    _protocol: u8,
    _skb: *mut SkBuff,
) -> i32 {
    // TODO: 实现协议查找
    -1 // 未实现
}

/// 初始化协议层。参考 C 的 `inet_proto_init()`。
pub fn init() {
    // 注册内置协议
    // - ICMP (1)
    // - TCP (6)
    // - UDP (17)
}
