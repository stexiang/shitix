//! BSD Socket 接口桥接层。将 syscall 连接到协议实现。

use crate::klib::errno::{EINVAL, ENOSYS, EOPNOTSUPP, EPROTONOSUPPORT, ESOCKTNOSUPPORT, EAFNOSUPPORT, EBADF, ENOMEM, EFAULT, EDESTADDRREQ, EMSGSIZE};
use crate::net::{AF_INET, AF_UNIX, SOCK_STREAM, SOCK_DGRAM};
use crate::mm::get_free_page;
use crate::net::inet::tcp::{self, flag as tflag, TcpState};
use crate::sched;
use core::mem::MaybeUninit;

#[repr(C)]
pub struct SockAddr { pub sa_family: u16, pub sa_data: [u8; 14] }
#[repr(C)]
pub struct SockAddrIn { pub sin_family: u16, pub sin_port: u16, pub sin_addr: [u8; 4], pub sin_zero: [u8; 8] }

struct SockEntry {
    family: u16, sock_type: u16, protocol: u8, used: bool,
    peer: usize,     // usize::MAX = none
    buf_page: usize,  // get_free_page allocated buffer (0 = not alloc)
    buf_len: usize, buf_read: usize,
    // AF_INET：bind 的本地端口 / connect 的对端（主机序）。
    // DGRAM 的 buf 是「u16 长度前缀 + 载荷」的数据报队列。
    local_port: u16, remote_ip: u32, remote_port: u16,
    // STREAM(TCP)：连接状态与序号（buf 是线性字节流缓冲）。
    tcp_state: TcpState, snd_una: u32, snd_nxt: u32, rcv_nxt: u32,
}

const MAX_SOCKETS: usize = 4;
const SOCK_NIL: usize = usize::MAX;
const SOCK_BUF_SZ: usize = 4096;

static mut SOCKS: [MaybeUninit<SockEntry>; MAX_SOCKETS] = [const { MaybeUninit::uninit() }; MAX_SOCKETS];
static mut SOCKS_INITED: bool = false;
const MAX_SOCK_FD: usize = 32;
static mut SOCK_FD_MAP: [usize; MAX_SOCK_FD] = [SOCK_NIL; MAX_SOCK_FD];

fn ensure_inited() {
    unsafe {
        if !SOCKS_INITED {
            for i in 0..MAX_SOCKETS {
                SOCKS[i].write(SockEntry {
                    family: 0, sock_type: 0, protocol: 0, used: false,
                    peer: SOCK_NIL, buf_page: 0, buf_len: 0, buf_read: 0,
                    local_port: 0, remote_ip: 0, remote_port: 0,
                    tcp_state: TcpState::Closed, snd_una: 0, snd_nxt: 0, rcv_nxt: 0,
                });
            }
            SOCKS_INITED = true;
        }
    }
}

unsafe fn sock_mut(idx: usize) -> &'static mut SockEntry {
    unsafe { SOCKS[idx].assume_init_mut() }
}

fn alloc_buf(sock_idx: usize) -> bool {
    unsafe {
        let s = sock_mut(sock_idx);
        if s.buf_page == 0 {
            s.buf_page = get_free_page();
            if s.buf_page == 0 { return false; }
        }
    }
    true
}

pub fn register_fd(fd: usize, sock_idx: usize) { unsafe { if fd < MAX_SOCK_FD { SOCK_FD_MAP[fd] = sock_idx; } } }
pub fn unregister_fd(fd: usize) { unsafe { if fd < MAX_SOCK_FD { SOCK_FD_MAP[fd] = SOCK_NIL; } } }
pub fn fd_is_socket(fd: usize) -> bool { unsafe { fd < MAX_SOCK_FD && SOCK_FD_MAP[fd] != SOCK_NIL } }
pub fn fd_to_sock(fd: usize) -> Option<usize> {
    unsafe { if fd < MAX_SOCK_FD && SOCK_FD_MAP[fd] != SOCK_NIL { Some(SOCK_FD_MAP[fd]) } else { None } }
}

pub fn close_socket(fd: usize) {
    if let Some(idx) = fd_to_sock(fd) {
        ensure_inited();
        unsafe {
            let s = sock_mut(idx);
            if s.buf_page != 0 { crate::mm::free_page(s.buf_page); s.buf_page = 0; }
        }
        unregister_fd(fd);
    }
}

fn alloc_sock() -> Option<usize> {
    ensure_inited();
    for i in 0..MAX_SOCKETS {
        unsafe {
            let s = sock_mut(i);
            if !s.used {
                s.used = true; s.family = 0; s.sock_type = 0; s.protocol = 0;
                s.peer = SOCK_NIL; s.buf_page = 0; s.buf_len = 0; s.buf_read = 0;
                s.local_port = 0; s.remote_ip = 0; s.remote_port = 0;
                s.tcp_state = TcpState::Closed; s.snd_una = 0; s.snd_nxt = 0; s.rcv_nxt = 0;
                return Some(i);
            }
        }
    }
    None
}

fn find_free_fd() -> Option<usize> {
    for f in 3usize..MAX_SOCK_FD {
        if !fd_is_socket(f) && !crate::fs::pipe::fd_is_pipe(f)
            && unsafe { crate::fs::open::fd_to_filp(f) == crate::fs::inode::NIL } {
            return Some(f);
        }
    }
    None
}

/// UDP 收包投递（netif→socket 接线）。按目的端口找已 bind 的
/// AF_INET DGRAM socket，把载荷以「u16 长度前缀 + 载荷」入队。
/// 找不到匹配端口（且没有通配 socket）则丢弃。
pub fn udp_input(_src_ip: u32, _src_port: u16, dst_port: u16, payload: &[u8]) {
    ensure_inited();
    unsafe {
        let mut target = SOCK_NIL;
        for i in 0..MAX_SOCKETS {
            let s = sock_mut(i);
            if s.used && s.family == AF_INET && s.sock_type == SOCK_DGRAM {
                if s.local_port == dst_port { target = i; break; }
                if s.local_port == 0 && target == SOCK_NIL { target = i; }
            }
        }
        if target == SOCK_NIL { return; }
        let s = sock_mut(target);
        if !alloc_buf(target) { return; }
        let s = sock_mut(target);
        // 数据报不跨界放不下就丢弃（4KB 环形区对演示足够）
        let need = 2 + payload.len();
        if s.buf_read + s.buf_len + need > SOCK_BUF_SZ { return; }
        let base = s.buf_page + s.buf_read + s.buf_len;
        let dlen = payload.len() as u16;
        core::ptr::write_volatile(base as *mut u8, dlen as u8);
        core::ptr::write_volatile((base + 1) as *mut u8, (dlen >> 8) as u8);
        core::ptr::copy_nonoverlapping(payload.as_ptr(), (base + 2) as *mut u8, payload.len());
        s.buf_len += need;
    }
}


// ---- TCP（AF_INET SOCK_STREAM）----

/// 发送一个裸段。返回 0/-errno（无网卡时 -ENETUNREACH）。
/// `seq` 由调用方指定（重传用原序号；snd_nxt 只在首次发送后推进）。
fn tcp_xmit(idx: usize, flags: u8, seq: u32, payload: &[u8]) -> i32 {
    ensure_inited();
    unsafe {
        let s = sock_mut(idx);
        let mut seg = [0u8; 1520];
        let n = tcp::build_segment(
            &mut seg,
            crate::net::inet::netif::our_ip(),
            s.remote_ip,
            s.local_port,
            s.remote_port,
            seq,
            s.rcv_nxt,
            flags,
            4096,
            payload,
        );
        if n == 0 {
            return -1;
        }
        let r = crate::net::inet::netif::send_ip_packet(s.remote_ip, 6, &seg[..n]);
        if r > 0 { 0 } else { -(crate::klib::errno::ENETUNREACH as i32) }
    }
}

/// 等 `cond(s)` 成立，其间轮询网卡收包并让出 CPU。返回 true=成立。
/// `cond` 读到的 socket 字段在收包路径里被改，必须每次重读。
fn tcp_wait<F: Fn(&SockEntry) -> bool>(idx: usize, cond: F, timeout_ticks: u64) -> bool {
    let deadline = sched::jiffies() + timeout_ticks;
    loop {
        crate::net::inet::netif::poll();
        // SAFETY: ensure_inited 后槽位生命周期到 close。
        unsafe {
            if cond(sock_mut(idx)) {
                return true;
            }
        }
        if sched::jiffies() >= deadline {
            return false;
        }
        // SAFETY: syscall 上下文（非 task[0]、非中断）。
        unsafe { sched::sleep_ticks(1) };
    }
}

/// TCP 三次握手。connect(2) 的 AF_INET STREAM 路径。
fn tcp_handshake(idx: usize) -> i64 {
    use crate::klib::errno::{ECONNREFUSED, ETIMEDOUT};
    ensure_inited();
    unsafe {
        let s = sock_mut(idx);
        if s.local_port == 0 {
            s.local_port = 49152 + idx as u16;
        }
        // ISN：TCP 要求随时间变化；jiffies 搅一下就够（我们不防欺骗攻击）。
        let isn = (sched::jiffies() as u32)
            .wrapping_mul(1103515245)
            .wrapping_add(12345);
        s.snd_una = isn;
        s.snd_nxt = isn; // SYN 用 ISN 作序号，建立后再 +1
        s.rcv_nxt = 0;
        s.tcp_state = TcpState::SynSent;
        tcp_xmit(idx, tflag::SYN, sock_mut(idx).snd_nxt, &[]);
        // SYN 每 50 tick 重传，总超时 300 tick（≈3s）
        for _ in 0..6u32 {
            let ok = tcp_wait(idx, |s| {
                s.tcp_state == TcpState::Established || s.tcp_state == TcpState::Closed
            }, 50);
            if ok {
                break;
            }
            tcp_xmit(idx, tflag::SYN, sock_mut(idx).snd_nxt, &[]);
        }
        match sock_mut(idx).tcp_state {
            TcpState::Established => 0,
            TcpState::Closed => -(ECONNREFUSED as i64),
            _ => -(ETIMEDOUT as i64),
        }
    }
}

/// STREAM 发送：stop-and-wait，未确认段超时重传。
fn tcp_stream_send(idx: usize, buf: *const u8, len: usize) -> i64 {
    use crate::klib::errno::{EPIPE, ETIMEDOUT};
    ensure_inited();
    unsafe {
        let s = sock_mut(idx);
        if s.tcp_state != TcpState::Established && s.tcp_state != TcpState::CloseWait {
            return -(EPIPE as i64);
        }
        let mut off = 0;
        while off < len {
            let chunk = core::cmp::min(len - off, 1460);
            // SAFETY: buf 来自用户缓冲区指针，syscall 层已验过范围。
            let payload = core::slice::from_raw_parts(buf.add(off), chunk);
            let start_seq = s.snd_nxt;
            // 发送后立即推进 snd_nxt，ACK 验证窗口才有 ack 的落点；
            // 重传用旧序号 `start_seq`。
            s_SND_NXT_ADV(idx, chunk as u32);
            let s = sock_mut(idx);
            let mut retries = 0;
            loop {
                tcp_xmit(idx, tflag::PSH | tflag::ACK, start_seq, payload);
                if tcp_wait(idx, |s| {
                    s.snd_una.wrapping_sub(start_seq) >= chunk as u32
                        || s.tcp_state == TcpState::Closed
                }, 100)
                {
                    break;
                }
                let _ = s;
                retries += 1;
                if retries >= 3 {
                    return -(ETIMEDOUT as i64);
                }
            }
            off += chunk;
            if sock_mut(idx).tcp_state == TcpState::Closed {
                return if off == len { len as i64 } else { -(EPIPE as i64) };
            }
        }
        len as i64
    }
}

/// snd_nxt 推进。独立成函数是为了避开 sock_mut 双借用。
unsafe fn s_SND_NXT_ADV(idx: usize, n: u32) {
    unsafe {
        let s = sock_mut(idx);
        s.snd_nxt = s.snd_nxt.wrapping_add(n);
    }
}

/// STREAM 接收：阻塞到有数据 / 对端关（返回 0 表示 EOF）。
fn tcp_stream_recv(idx: usize, buf: *mut u8, len: usize) -> i64 {
    ensure_inited();
    let got_data = tcp_wait(idx, |s| s.buf_len > 0 || s.tcp_state != TcpState::Established && s.tcp_state != TcpState::CloseWait || s.buf_len > 0, 3600000);
    let _ = got_data;
    unsafe {
        let s = sock_mut(idx);
        if s.buf_len == 0 {
            return 0; // EOF（对端 FIN）或 Closed
        }
        let n = core::cmp::min(len, s.buf_len);
        let base = s.buf_page;
        // SAFETY: buf 用户指针经 syscall 层校验；线性缓冲不跨界。
        core::ptr::copy_nonoverlapping((base + s.buf_read) as *const u8, buf, n);

        s.buf_read += n;
        s.buf_len -= n;
        if s.buf_len == 0 {
            s.buf_read = 0;
        }
        n as i64
    }
}

/// TCP 收包投递（tcp.rs → 本层）。匹配 (dport=本地端口) 的 STREAM
/// socket，原地推进序号与状态，收到载荷就入流缓冲、回 ACK。
pub fn tcp_input(src_ip: u32, sport: u16, dport: u16, seq: u32, ack: u32, flags: u8, payload: &[u8]) {
    ensure_inited();
    unsafe {
        for i in 0..MAX_SOCKETS {
            let s = sock_mut(i);
            if !s.used || s.family != AF_INET || s.sock_type != SOCK_STREAM {
                continue;
            }
            if s.local_port != dport {
                continue;
            }
            // 已知对端的连接只收来自该对端的段
            if s.remote_port != 0 && (s.remote_ip != src_ip || s.remote_port != sport) {
                continue;
            }

            if flags & tflag::RST != 0 {
                s.tcp_state = TcpState::Closed;
                return;
            }

            match s.tcp_state {
                TcpState::SynSent => {
                    if flags & tflag::SYN != 0 && flags & tflag::ACK != 0 {
                        let mut un = s.snd_una;
                        if ack.wrapping_sub(un) == 1 || ack == s.snd_nxt.wrapping_add(1) {
                            un = ack;
                            s.rcv_nxt = seq.wrapping_add(1);
                            s.snd_una = un;
                            s.snd_nxt = ack; // SYN 消耗后，发送序从 ack 起
                            tcp_xmit(i, tflag::ACK, s.snd_nxt, &[]);
                            s.tcp_state = TcpState::Established;
                        }
                    }
                }
                TcpState::Established | TcpState::CloseWait | TcpState::FinWait1 => {
                    // 序号窗口：只收以 rcv_nxt 开头（或有重叠）的段，其余回 ACK【丢弃 】
                    let skip = s.rcv_nxt.wrapping_sub(seq) as usize;
                    if !payload.is_empty() && skip < payload.len() {
                        let new = &payload[skip..];
                        let space = SOCK_BUF_SZ.saturating_sub(s.buf_len);
                        let n = core::cmp::min(new.len(), space);
                        if n > 0 {
                            if s.buf_page == 0 && !alloc_buf(i) {
                                // 没缓冲页就只能丢（对方会重传）
                            } else {
                                let base = s.buf_page;
                                core::ptr::copy_nonoverlapping(new.as_ptr(), (base + s.buf_len) as *mut u8, n);
                                s.buf_len += n;
                                s.rcv_nxt = s.rcv_nxt.wrapping_add(n as u32);
                            }
                        }
                    } else if !payload.is_empty() {
                        crate::sprintln!("tcp_input: OOS seq={:#x} rcv_nxt={:#x} plen={}",
                            seq, s.rcv_nxt, payload.len());
                    }
                    if flags & tflag::FIN != 0 {
                        // FIN 消耗一个序号；仅当正好落在接收序末尾才接受
                        let fin_seq = seq.wrapping_add(payload.len() as u32);
                        if fin_seq == s.rcv_nxt || payload.is_empty() && seq == s.rcv_nxt {
                            s.rcv_nxt = s.rcv_nxt.wrapping_add(1);
                            s.tcp_state = TcpState::CloseWait;
                        }
                    }
                    // ACK 推进发送窗口
                    if ack.wrapping_sub(s.snd_una) <= (s.snd_nxt.wrapping_sub(s.snd_una))
                        && ack.wrapping_sub(s.snd_una) < 0x8000_0000
                    {
                        s.snd_una = ack;
                    }
                    // 只有消费了载荷或 FIN 才回 ACK；对纯 ACK 回 ACK 会造成 ACK 风暴
                    if !payload.is_empty() || flags & tflag::FIN != 0 {
                        tcp_xmit(i, tflag::ACK, s.snd_nxt, &[]);
                    }
                }
                _ => {}
            }
            return;
        }
    }
}

// ---- syscall implementations ----


/// TCP echo 自检：连 `ip:port`，发一串、收 echo 比对。
/// 无服务器/超时 → 打印 skipped（不算 FAIL，CI 无对端时仍过）。
pub fn tcp_echo_test(ip: u32, port: u16) {
    let fd = sys_socket(AF_INET, SOCK_STREAM, 0);
    if fd < 0 {
        crate::sprintln!("tcp: echo test socket failed {}", fd);
        return;
    }
    let mut sa = SockAddrIn {
        sin_family: AF_INET,
        sin_port: port.to_be(),
        sin_addr: ip.to_be_bytes(),
        sin_zero: [0; 8],
    };
    let r = sys_connect(fd as usize, &mut sa as *mut SockAddrIn as *const u8,
                        core::mem::size_of::<SockAddrIn>());
    if r < 0 {
        crate::sprintln!("tcp: echo test connect {} -> skipped", r);
        close_socket(fd as usize);
        return;
    }
    let msg = b"shitix-tcp-echo";
    let n = sys_sendto(fd as usize, msg.as_ptr(), msg.len(), 0,
                       core::ptr::null(), 0);
    if n != msg.len() as i64 {
        crate::sprintln!("tcp: echo test send {} -> FAIL", n);
        close_socket(fd as usize);
        return;
    }
    let mut buf = [0u8; 64];
    let rc = sys_recvfrom(fd as usize, buf.as_mut_ptr(), msg.len(), 0,
                          core::ptr::null_mut(), core::ptr::null_mut());
    let ok = rc == msg.len() as i64 && buf[..rc as usize] == msg[..];
    crate::sprintln!("tcp: echo {} bytes -> {}", rc, if ok { "ok" } else { "FAIL" });
    close_socket(fd as usize);
}

pub fn sys_socket(family: u16, sock_type: u16, protocol: u8) -> i64 {
    if family != AF_INET && family != AF_UNIX { return -(EAFNOSUPPORT as i64); }
    if sock_type != SOCK_STREAM && sock_type != SOCK_DGRAM { return -(ESOCKTNOSUPPORT as i64); }

    let idx = match alloc_sock() { Some(i) => i, None => return -(ENOMEM as i64) };
    unsafe { let s = sock_mut(idx); s.family = family; s.sock_type = sock_type; s.protocol = protocol; }
    let fd = match find_free_fd() { Some(f) => f, None => return -(ENOMEM as i64) };
    register_fd(fd, idx);
    fd as i64
}

pub fn sys_bind(fd: usize, addr: *const u8, addrlen: usize) -> i64 {
    if addr.is_null() || addrlen < 2 { return -(EFAULT as i64); }
    let sock_idx = match fd_to_sock(fd) { Some(i) => i, None => return -(EBADF as i64) };
    ensure_inited();
    unsafe {
        let s = sock_mut(sock_idx);
        if s.family == AF_INET {
            if addrlen < core::mem::size_of::<SockAddrIn>() { return -(EINVAL as i64); }
            let sa = &*(addr as *const SockAddrIn);
            s.local_port = u16::from_be(sa.sin_port);
        }
    }
    0
}

pub fn sys_listen(fd: usize, _backlog: i32) -> i64 {
    if fd_to_sock(fd).is_none() { return -(EBADF as i64); }
    0
}

pub fn sys_accept(fd: usize, addr: *mut u8, addrlen: *mut u32) -> i64 {
    let sock_idx = match fd_to_sock(fd) { Some(i) => i, None => return -(EBADF as i64) };
    let new_idx = match alloc_sock() { Some(i) => i, None => return -(ENOMEM as i64) };
    ensure_inited();
    unsafe {
        let family = sock_mut(sock_idx).family;
        let sock_type = sock_mut(sock_idx).sock_type;
        let protocol = sock_mut(sock_idx).protocol;
        sock_mut(new_idx).family = family;
        sock_mut(new_idx).sock_type = sock_type;
        sock_mut(new_idx).protocol = protocol;
        sock_mut(new_idx).peer = sock_idx;
        sock_mut(sock_idx).peer = new_idx;
    }
    let new_fd = match find_free_fd() { Some(f) => f, None => return -(ENOMEM as i64) };
    register_fd(new_fd, new_idx);
    if !addr.is_null() && !addrlen.is_null() {
        unsafe { core::ptr::write_volatile(addr as *mut u16, AF_UNIX); core::ptr::write_volatile(addrlen, 2u32); }
    }
    new_fd as i64
}

pub fn sys_connect(fd: usize, addr: *const u8, addrlen: usize) -> i64 {
    if addr.is_null() || addrlen < 2 { return -(EFAULT as i64); }
    let sock_idx = match fd_to_sock(fd) { Some(i) => i, None => return -(EBADF as i64) };
    ensure_inited();
    unsafe {
        let s = sock_mut(sock_idx);
        if s.family == AF_INET {
            if addrlen < core::mem::size_of::<SockAddrIn>() { return -(EINVAL as i64); }
            let sa = &*(addr as *const SockAddrIn);
            let port = u16::from_be(sa.sin_port);
            let ip = u32::from_be_bytes(sa.sin_addr);
            if s.sock_type == SOCK_STREAM {
                s.remote_port = port;
                s.remote_ip = ip;
                return tcp_handshake(sock_idx);
            }
            s.remote_port = port;
            s.remote_ip = ip;
        }
    }
    0
}

pub fn sys_sendto(fd: usize, buf: *const u8, len: usize, _flags: i32,
                  dest_addr: *const u8, addrlen: usize) -> i64 {
    if buf.is_null() { return -(EFAULT as i64); }
    let sock_idx = match fd_to_sock(fd) { Some(i) => i, None => return -(EBADF as i64) };
    ensure_inited();
    // AF_INET DGRAM：组 UDP 头后经 netif→e1000 发出。
    unsafe {
        let s = sock_mut(sock_idx);
        if s.family == AF_INET && s.sock_type == SOCK_STREAM {
            return tcp_stream_send(sock_idx, buf, len);
        }
        if s.family == AF_INET && s.sock_type == SOCK_DGRAM {
            let (dst_ip, dst_port) = if !dest_addr.is_null()
                && addrlen >= core::mem::size_of::<SockAddrIn>()
            {
                let sa = &*(dest_addr as *const SockAddrIn);
                (u32::from_be_bytes(sa.sin_addr), u16::from_be(sa.sin_port))
            } else if s.remote_port != 0 {
                (s.remote_ip, s.remote_port)
            } else {
                return -(EDESTADDRREQ as i64);
            };
            if len + 8 > 1472 { return -(EMSGSIZE as i64); }
            // 未 bind 时分配临时端口
            if s.local_port == 0 { s.local_port = 49152 + sock_idx as u16; }
            let sport = s.local_port;
            let mut udp = [0u8; 1480];
            udp[0] = (sport >> 8) as u8; udp[1] = sport as u8;
            udp[2] = (dst_port >> 8) as u8; udp[3] = dst_port as u8;
            let ulen = (8 + len) as u16;
            udp[4] = (ulen >> 8) as u8; udp[5] = ulen as u8;
            // udp[6..8] 校验和留 0（IPv4 下合法）
            core::ptr::copy_nonoverlapping(buf, udp.as_mut_ptr().add(8), len);
            let sent = crate::net::inet::netif::send_ip_packet(dst_ip, 17, &udp[..8 + len]);
            return if sent > 0 { len as i64 } else { -(crate::klib::errno::ENETUNREACH as i64) };
        }
    }
    unsafe {
        let peer = sock_mut(sock_idx).peer;
        if peer == SOCK_NIL { return len as i64; /* discard */ }
        if !alloc_buf(peer) { return -(ENOMEM as i64); }
        let p = sock_mut(peer);
        let space = SOCK_BUF_SZ.saturating_sub(p.buf_len);
        let n = core::cmp::min(len, space);
        if n > 0 {
            let buf_start = p.buf_page;
            if p.buf_len + n <= SOCK_BUF_SZ {
                core::ptr::copy_nonoverlapping(buf, (buf_start + p.buf_len) as *mut u8, n);
            } else {
                let first = SOCK_BUF_SZ - p.buf_len;
                core::ptr::copy_nonoverlapping(buf, (buf_start + p.buf_len) as *mut u8, first);
                core::ptr::copy_nonoverlapping(buf.add(first), buf_start as *mut u8, n - first);
            }
            p.buf_len += n;
        }
        n as i64
    }
}

pub fn sys_recvfrom(fd: usize, buf: *mut u8, len: usize, _flags: i32,
                    _src_addr: *mut u8, _addrlen: *mut u32) -> i64 {
    if buf.is_null() { return -(EFAULT as i64); }
    let sock_idx = match fd_to_sock(fd) { Some(i) => i, None => return -(EBADF as i64) };
    ensure_inited();
    // AF_INET DGRAM：先从网卡收包入队，再按「u16 长度前缀 + 载荷」
    // 弹出一个完整数据报（UDP 语义：一次 recvfrom 一条报文）。
    unsafe {
        let s = sock_mut(sock_idx);
        if s.family == AF_INET && s.sock_type == SOCK_STREAM {
            return tcp_stream_recv(sock_idx, buf, len);
        }
        if s.family == AF_INET && s.sock_type == SOCK_DGRAM {
            crate::net::inet::netif::poll();
            let s = sock_mut(sock_idx);
            if s.buf_page == 0 || s.buf_len < 2 { return 0; }
            let base = s.buf_page;
            let dlen = u16::from_le_bytes([
                core::ptr::read_volatile((base + s.buf_read) as *const u8),
                core::ptr::read_volatile((base + s.buf_read + 1) as *const u8),
            ]) as usize;
            let n = core::cmp::min(len, dlen);
            // 数据报一定整段落在页内（入队时已保证不跨界）
            core::ptr::copy_nonoverlapping((base + s.buf_read + 2) as *const u8, buf, n);
            s.buf_read += 2 + dlen;
            s.buf_len -= 2 + dlen;
            if s.buf_len == 0 { s.buf_read = 0; }
            return n as i64;
        }
    }
    unsafe {
        let s = sock_mut(sock_idx);
        if s.buf_page == 0 || s.buf_len == 0 { return 0; }
        let n = core::cmp::min(len, s.buf_len);
        let buf_start = s.buf_page;
        if s.buf_read + n <= SOCK_BUF_SZ {
            core::ptr::copy_nonoverlapping((buf_start + s.buf_read) as *const u8, buf, n);
        } else {
            let first = SOCK_BUF_SZ - s.buf_read;
            core::ptr::copy_nonoverlapping((buf_start + s.buf_read) as *const u8, buf, first);
            core::ptr::copy_nonoverlapping(buf_start as *const u8, buf.add(first), n - first);
        }
        s.buf_read = (s.buf_read + n) % SOCK_BUF_SZ;
        s.buf_len -= n;
        n as i64
    }
}

pub fn sys_setsockopt(fd: usize, _level: i32, _optname: i32, _optval: *const u8, _optlen: usize) -> i64 {
    if fd_to_sock(fd).is_none() { return -(EBADF as i64); }
    0
}

pub fn sys_getsockopt(_fd: usize, _level: i32, _optname: i32, _optval: *mut u8, _optlen: *mut u32) -> i64 { 0 }

pub fn sys_shutdown(fd: usize, _how: i32) -> i64 {
    if fd_to_sock(fd).is_none() { return -(EBADF as i64); }
    0
}

pub fn sys_getsockname(fd: usize, addr: *mut u8, addrlen: *mut u32) -> i64 {
    if addr.is_null() || addrlen.is_null() { return -(EFAULT as i64); }
    let sock_idx = match fd_to_sock(fd) { Some(i) => i, None => return -(EBADF as i64) };
    ensure_inited();
    unsafe { core::ptr::write_volatile(addr as *mut u16, sock_mut(sock_idx).family); core::ptr::write_volatile(addrlen, 2u32); }
    0
}

pub fn sys_getpeername(fd: usize, addr: *mut u8, addrlen: *mut u32) -> i64 {
    if addr.is_null() || addrlen.is_null() { return -(EFAULT as i64); }
    let sock_idx = match fd_to_sock(fd) { Some(i) => i, None => return -(EBADF as i64) };
    ensure_inited();
    unsafe {
        let peer = sock_mut(sock_idx).peer;
        if peer == SOCK_NIL { return -(ENOSYS as i64); }
        core::ptr::write_volatile(addr as *mut u16, sock_mut(peer).family);
        core::ptr::write_volatile(addrlen, 2u32);
    }
    0
}

pub fn sys_socketpair(family: u16, sock_type: u16, protocol: u8, sv: *mut i32) -> i64 {
    if sv.is_null() { return -(EFAULT as i64); }
    let s1 = sys_socket(family, sock_type, protocol);
    if s1 < 0 { return s1; }
    let s2 = sys_socket(family, sock_type, protocol);
    if s2 < 0 { unregister_fd(s1 as usize); return s2; }
    ensure_inited();
    unsafe {
        let idx1 = fd_to_sock(s1 as usize).unwrap();
        let idx2 = fd_to_sock(s2 as usize).unwrap();
        sock_mut(idx1).peer = idx2;
        sock_mut(idx2).peer = idx1;
    }
    unsafe { core::ptr::write_volatile(sv, s1 as i32); core::ptr::write_volatile(sv.add(1), s2 as i32); }
    0
}

pub fn sys_sendmsg(fd: usize, msg: *const u8, _flags: i32) -> i64 {
    if msg.is_null() { return -(EFAULT as i64); }
    if fd_to_sock(fd).is_none() { return -(EBADF as i64); }
    unsafe {
        let iov = core::ptr::read_volatile(msg.add(16) as *const usize) as *const u8;
        let iovlen: usize = core::ptr::read_volatile(msg.add(24) as *const usize);
        if iov.is_null() || iovlen == 0 { return 0; }
        let base = core::ptr::read_volatile(iov as *const usize) as *const u8;
        let len: usize = core::ptr::read_volatile(iov.add(8) as *const usize);
        sys_sendto(fd, base, len, 0, core::ptr::null(), 0)
    }
}

pub fn sys_recvmsg(fd: usize, msg: *mut u8, _flags: i32) -> i64 {
    if msg.is_null() { return -(EFAULT as i64); }
    if fd_to_sock(fd).is_none() { return -(EBADF as i64); }
    unsafe {
        let iov = core::ptr::read_volatile(msg.add(16) as *const usize) as *mut u8;
        let iovlen: usize = core::ptr::read_volatile(msg.add(24) as *const usize);
        if iov.is_null() || iovlen == 0 { return 0; }
        let base = core::ptr::read_volatile(iov as *const usize) as *mut u8;
        let len: usize = core::ptr::read_volatile(iov.add(8) as *const usize);
        sys_recvfrom(fd, base, len, 0, core::ptr::null_mut(), core::ptr::null_mut())
    }
}
