//! INET 协议族。参考 `linux/net/inet/`。
//!
//! 实现 TCP/IP 协议栈：
//!
//! - **IP** (`ip.rs`)：网络层，提供无连接分组传递
//! - **TCP** (`tcp.rs`)：传输层，面向连接可靠字节流
//! - **UDP** (`udp.rs`)：传输层，无连接数据报
//! - **ICMP** (`icmp.rs`)：网络层，差错报告和诊断
//! - **ARP** (`arp.rs`)：链路层，IP → MAC 地址解析
//! - **Ethernet** (`eth.rs`)：链路层，以太网帧封装
//!
//! ## 内存布局
//!
//! 关键数据结构：
//!
//! - [`Socket`](sock::Socket)：BSD socket 的内核表示
//! - [`SkBuff`](skbuff::SkBuff)：网络数据包的缓冲区描述符
//!
//! ## C 源码对照
//!
//! | Rust 模块 | C 文件 | 说明 |
//! |-----------|--------|------|
//! | `sock.rs` | `net/inet/sock.c` | Socket 管理 |
//! | `skbuff.rs` | `net/inet/skbuff.c` | Socket buffer 管理 |
//! | `ip.rs` | `net/inet/ip.c` | IP 协议实现 |
//! | `tcp.rs` | `net/inet/tcp.c` | TCP 协议实现 |
//! | `udp.rs` | `net/inet/udp.c` | UDP 协议实现 |
//! | `icmp.rs` | `net/inet/icmp.c` | ICMP 协议实现 |
//! | `arp.rs` | `net/inet/arp.c` | ARP 协议实现 |
//! | `eth.rs` | `net/inet/eth.c` | 以太网封装 |
//! | `dev.rs` | `net/inet/dev.c` | 网络设备管理 |

pub mod sock;
pub mod skbuff;
pub mod ip;
pub mod tcp;
pub mod udp;
pub mod icmp;
pub mod arp;
pub mod eth;
pub mod dev;
pub mod route;
pub mod protocol;
pub mod netif;
