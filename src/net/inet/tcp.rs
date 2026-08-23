//! TCP 协议实现。参考 `linux/net/inet/tcp.c`。
//!
//! ## 功能
//!
//! 主动端（client）的最小可靠字节流：
//!
//! - 三次握手（SYN → SYN-ACK → ACK）
//! - stop-and-wait 数据发送（一个未确认段在飞，超时重传）
//! - 按序接收（乱序/重复段只回 ACK 丢弃，等对端重传）
//! - 被动关闭（对端 FIN → CloseWait，recv 返回 0）与主动 FIN
//!
//! 状态机用的是 [`TcpState`] 全集，但服务端路径（Listen/SynReceived）
//! 尚未接 socket——listen/accept 还是 AF_UNIX 的近似。
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `tcp.c` | TCP 协议实现 |
//! | `tcp.h` | TCP 头结构 |

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

/// 段头标志位（`tcphdr` 的 flags 字节）。
pub mod flag {
    pub const FIN: u8 = 0x01;
    pub const SYN: u8 = 0x02;
    pub const RST: u8 = 0x04;
    pub const PSH: u8 = 0x08;
    pub const ACK: u8 = 0x10;
    pub const URG: u8 = 0x20;
}

/// 解析输入段。返回 (sport, dport, seq, ack, flags, hdr_len)。
pub fn parse(data: &[u8]) -> Option<(u16, u16, u32, u32, u8, usize)> {
    if data.len() < 20 {
        return None;
    }
    let sport = u16::from_be_bytes([data[0], data[1]]);
    let dport = u16::from_be_bytes([data[2], data[3]]);
    let seq = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let ack = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let hdr_len = ((data[12] >> 4) * 4) as usize;
    if hdr_len < 20 || hdr_len > data.len() {
        return None;
    }
    let flags = data[13];
    Some((sport, dport, seq, ack, flags, hdr_len))
}

/// TCP 校验和（含 12 字节伪头部，参考 RFC 793 §3.1）。
fn checksum(src_ip: u32, dst_ip: u32, seg: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut acc = |b: &[u8]| {
        let mut i = 0;
        while i + 1 < b.len() {
            sum = sum.wrapping_add(u16::from_be_bytes([b[i], b[i + 1]]) as u32);
            i += 2;
        }
        if i < b.len() {
            sum = sum.wrapping_add((b[i] as u32) << 8);
        }
    };
    acc(&src_ip.to_be_bytes());
    acc(&dst_ip.to_be_bytes());
    acc(&[0u8, 6u8]);
    acc(&(seg.len() as u16).to_be_bytes());
    acc(seg);
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// 组装一个 TCP 段（含校验和），写入 `buf`，返回段长。固定 20 字节头（无选项）。
#[allow(clippy::too_many_arguments)]
pub fn build_segment(
    buf: &mut [u8],
    src_ip: u32,
    dst_ip: u32,
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    payload: &[u8],
) -> usize {
    let total = 20 + payload.len();
    if buf.len() < total {
        return 0;
    }
    buf[0..2].copy_from_slice(&sport.to_be_bytes());
    buf[2..4].copy_from_slice(&dport.to_be_bytes());
    buf[4..8].copy_from_slice(&seq.to_be_bytes());
    buf[8..12].copy_from_slice(&ack.to_be_bytes());
    buf[12] = 5 << 4; // data offset = 5×4 字节
    buf[13] = flags;
    buf[14..16].copy_from_slice(&window.to_be_bytes());
    buf[16] = 0; // checksum 占位
    buf[17] = 0;
    buf[18] = 0; // urg ptr
    buf[19] = 0;
    buf[20..total].copy_from_slice(payload);
    let csum = checksum(src_ip, dst_ip, &buf[..total]);
    buf[16..18].copy_from_slice(&csum.to_be_bytes());
    total
}

/// 处理收到的 TCP 段（netif 从 IP 层调上来）。解析后路由到 socket 层
/// 的连接表（状态迁移/序号推进都在 `net::socket` 的连接条目里做）。
pub fn tcp_input(src_ip: u32, data: &[u8]) {
    let Some((sport, dport, seq, ack, flags, hdr_len)) = parse(data) else {
        return;
    };
    crate::net::socket::tcp_input(src_ip, sport, dport, seq, ack, flags, &data[hdr_len..]);
}

/// 初始化 TCP 层。参考 C 的 `tcp_init()`。
pub fn init() {}
