//! BSD Socket 接口桥接层。将 syscall 连接到协议实现。

use crate::klib::errno::{EINVAL, ENOSYS, EOPNOTSUPP, EPROTONOSUPPORT, ESOCKTNOSUPPORT, EAFNOSUPPORT, EBADF, ENOMEM, EFAULT};
use crate::net::{AF_INET, AF_UNIX, SOCK_STREAM, SOCK_DGRAM};
use crate::mm::get_free_page;
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
pub fn fd_is_socket(fd: usize) -> bool { unsafe { fd < 64 && SOCK_FD_MAP[fd] != SOCK_NIL } }
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

// ---- syscall implementations ----

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
    if fd_to_sock(fd).is_none() { return -(EBADF as i64); }
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
    if fd_to_sock(fd).is_none() { return -(EBADF as i64); }
    0
}

pub fn sys_sendto(fd: usize, buf: *const u8, len: usize, _flags: i32,
                  _dest_addr: *const u8, _addrlen: usize) -> i64 {
    if buf.is_null() { return -(EFAULT as i64); }
    let sock_idx = match fd_to_sock(fd) { Some(i) => i, None => return -(EBADF as i64) };
    ensure_inited();
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
