//! 网络协议栈。参考 linux-1.0.9 的 `linux/net/`。
//!
//! ## 架构
//!
//! ```text
//! +------------------+
//! |     BSD Socket    |  <- 用户空间 API（syscall）
//! +------------------+
//!         |
//! +------------------+
//! |   Protocol       |
//! |   Families       |
//! |  (inet, unix)   |
//! +------------------+
//!         |
//! +------------------+
//! |   Transport      |
//! |  (TCP, UDP, ICMP)|
//! +------------------+
//!         |
//! +------------------+
//! |   Network        |
//! |   (IP, ARP)      |
//! +------------------+
//!         |
//! +------------------+
//! |   Device         |
//! |   Drivers        |
//! +------------------+
//! ```
//!
//! ## C 源码对照
//!
//! - `linux/net/socket.c` → BSD socket 接口
//! - `linux/net/inet/` → TCP/IP/UDP/ICMP/ARP 协议实现
//! - `linux/net/unix/` → Unix 域套接字
//! - `linux/drivers/net/` → 网卡驱动（NE2000、3COM、SLIP 等）
//!
//! ## Safety 约定
//!
//! 网络栈涉及大量底层操作，需要 `unsafe`：
//!
//! - 访问硬件寄存器（网卡 DMA 缓冲区）
//! - 直接操作 DMA 内存
//! - 中断上下文的数据访问
//! - 跨 CPU 缓存同步
//!
//! **每处 `unsafe` 必须有 `SAFETY` 注释**，解释为什么在该上下文下是安全的。
//!
//! ## 地址族（Address Families）
//!
//! | AF_* | 名称 | 说明 |
//! |------|------|------|
//! | AF_UNSPEC | 未指定 | 占位符 |
//! | AF_INET | IPv4 | TCP/UDP/ICMP over IP |
//! | AF_UNIX | Unix | 本地进程间通信 |
//!
//! ## Socket 类型
//!
//! | SOCK_* | 说明 |
//! |--------|------|
//! | SOCK_STREAM | 面向连接（TCP）|
//! | SOCK_DGRAM | 无连接（UDP）|
//! | SOCK_RAW | 原始套接字（跳过传输层）|

pub mod inet;
pub mod socket;
pub mod unix;

// 测试模块（始终可用，在内核启动时自检）
pub mod tests;

pub use inet::sock::Socket;
pub use inet::skbuff::SkBuff;

/// Socket address families. 对应 `linux/include/linux/socket.h` 的 `AF_*`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum AddressFamily {
    Unspec = 0,
    Unix = 1,
    Inet = 2,
    Ax25 = 3,
    Ipx = 4,
}

/// Socket types. 对应 `linux/include/linux/socket.h` 的 `SOCK_*`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SocketType {
    Stream = 1,    // SOCK_STREAM
    Dgram = 2,     // SOCK_DGRAM
    Raw = 3,       // SOCK_RAW
    Rdm = 4,       // SOCK_RDM
    SeqPacket = 5,  // SOCK_SEQPACKET
    Packet = 10,    // SOCK_PACKET (Linux specific)
}

/// Protocol families. 通常与地址族相同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ProtocolFamily {
    Unix = 1,
    Inet = 2,
    Ax25 = 3,
    Ipx = 4,
}

/// Socket domain/address family.
pub const AF_UNSPEC: u16 = 0;
pub const AF_UNIX: u16 = 1;
pub const AF_INET: u16 = 2;
pub const AF_AX25: u16 = 3;
pub const AF_IPX: u16 = 4;

/// Socket types.
pub const SOCK_STREAM: u16 = 1;
pub const SOCK_DGRAM: u16 = 2;
pub const SOCK_RAW: u16 = 3;
pub const SOCK_RDM: u16 = 4;
pub const SOCK_SEQPACKET: u16 = 5;
pub const SOCK_PACKET: u16 = 10;

/// Protocol numbers (IPPROTO_* in C).
pub const IPPROTO_IP: u8 = 0;
pub const IPPROTO_ICMP: u8 = 1;
pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;
