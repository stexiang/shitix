//! TCP 协议实现。参考 `linux/net/inet/tcp.c`。
//!
//! ## 功能
//!
//! - 面向连接可靠字节流
//! - 流量控制（滑动窗口）
//! - 拥塞控制
//! - 保活机制
//!
//! ## TCP 状态机
//!
//! ```text
//! CLOSED → SYN_SENT → ESTABLISHED → FIN_WAIT_1 → FIN_WAIT_2 → TIME_WAIT → CLOSED
//!                ↓                    ↓
//!            SYN_RECV ←←←←←←←←←←←←←←←←←←
//!                ↓
//!           CLOSE_WAIT → LAST_ACK → CLOSED
//! ```
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `tcp.c` | TCP 协议实现 |
//! | `tcp.h` | TCP 头结构 |
//!
//! ## SAFETY
//!
//! TCP 是最复杂的协议，需要大量 `unsafe`：
//!
//! - 定时器操作（`timer.c`）
//! - 序列号运算（u32 溢出处理）
//! - 内存分配/释放
//! - 锁操作

use crate::net::inet::skbuff::{self, SkBuff, TcpHeader, SkBuffQueue};

/// TCP 状态。参考 C 的 `volatile unsigned char state`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TcpState {
    Closed = 0,
    Listen = 1,
    SynSent = 2,
    SynReceived = 3,
    Established = 4,
    CloseWait = 5,
    FinWait1 = 6,
    Closing = 7,
    LastAck = 8,
    FinWait2 = 9,
    TimeWait = 10,
}

/// TCP 选项。
pub const TCP_NODELAY: u8 = 1;
pub const TCP_MAXSEG: u8 = 2;

/// 初始化 TCP 层。参考 C 的 `tcp_init()`。
pub fn init() {
    // 初始化 TCP 哈希表、定时器等
}

/// 处理输入的 TCP 包。参考 C 的 `tcp_rcv()`。
///
/// # Safety
///
/// - `skb` 必须有效
/// - 可能从中断上下文调用
pub unsafe fn tcp_rcv(skb: *mut SkBuff) -> i32 {
    // SAFETY: 调用者保证 `skb` 有效。
    unsafe {
        let hdr = (*skb).tcp_header();
        
        // 解析 TCP 头
        let sport = u16::from_be(hdr.source);
        let dport = u16::from_be(hdr.dest);
        let seq = u32::from_be(hdr.seq);
        let ack = u32::from_be(hdr.ack_seq);
        let flags = hdr.doff_flags >> 8;
        
        // TODO: 完整的状态机处理
        
        0
    }
}

/// TCP 发送。参考 C 的 `tcp_write()`。
///
/// # Safety
///
/// - socket 必须有效且处于可写状态
pub unsafe fn tcp_send(sk: *mut crate::net::inet::sock::Socket, data: &[u8]) -> i32 {
    // SAFETY: 调用者保证 `sk` 有效。
    unsafe {
        // TODO: 实现
        0
    }
}
