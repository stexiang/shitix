//! 管道实现。参考 linux-1.0.9 的 `fs/pipe.c`。
//! 使用动态分配的管道结构体和缓冲区以节省 BSS。

use crate::klib::errno::{EFAULT, EPIPE};
use crate::mm::{get_free_page, free_page};
use crate::sched::WaitQueue;

pub const PIPE_BUF_SIZE: usize = 4096;
const MAX_PIPES: usize = 4;
const MAX_PIPE_FD: usize = 32;
const NIL: usize = usize::MAX;

/// Pipe allocations: each active pipe gets a page (first 64B = metadata, rest = ring buffer).
/// Pipe page layout: [PipeMeta 64B][ring buffer PIPE_BUF_SIZE-64B]
struct PipeMeta {
    len: usize,
    read_pos: usize,
    write_pos: usize,
    writers: u8,
    readers: u8,
    used: bool,
    _pad: [u8; 3],
    read_wait: WaitQueue,
    write_wait: WaitQueue,
}

static mut PIPE_PAGES: [usize; MAX_PIPES] = [0; MAX_PIPES];
static mut PIPE_FD_MAP: [usize; MAX_PIPE_FD] = [NIL; MAX_PIPE_FD];

unsafe fn meta(page: usize) -> *mut PipeMeta {
    page as *mut PipeMeta
}

pub fn alloc_pipe() -> Option<usize> {
    unsafe {
        for i in 0..MAX_PIPES {
            if PIPE_PAGES[i] == 0 {
                let page = get_free_page();
                if page == 0 { return None; }
                let m = meta(page);
                (*m).len = 0;
                (*m).read_pos = 0;
                (*m).write_pos = 0;
                (*m).writers = 1;
                (*m).readers = 1;
                (*m).used = true;
                (*m).read_wait = WaitQueue::new();
                (*m).write_wait = WaitQueue::new();
                PIPE_PAGES[i] = page;
                return Some(i);
            }
        }
    }
    None
}

pub fn register_fd(fd: usize, pipe_idx: usize) {
    unsafe { if fd < MAX_PIPE_FD { PIPE_FD_MAP[fd] = pipe_idx; } }
}

pub fn unregister_fd(fd: usize) {
    unsafe { if fd < MAX_PIPE_FD { PIPE_FD_MAP[fd] = NIL; } }
}

pub fn fd_is_pipe(fd: usize) -> bool {
    unsafe { fd < MAX_PIPE_FD && PIPE_FD_MAP[fd] != NIL }
}

pub fn fd_to_pipe(fd: usize) -> Option<usize> {
    unsafe {
        if fd < MAX_PIPE_FD && PIPE_FD_MAP[fd] != NIL {
            Some(PIPE_FD_MAP[fd])
        } else { None }
    }
}

pub fn close_reader(fd: usize) {
    if let Some(idx) = fd_to_pipe(fd) {
        unsafe {
            let page = PIPE_PAGES[idx];
            if page != 0 {
                let m = meta(page);
                (*m).readers = (*m).readers.saturating_sub(1);
                if (*m).readers == 0 { (*m).write_wait.wake_up(); }
            }
        }
    }
}

pub fn close_writer(fd: usize) {
    if let Some(idx) = fd_to_pipe(fd) {
        unsafe {
            let page = PIPE_PAGES[idx];
            if page != 0 {
                let m = meta(page);
                (*m).writers = (*m).writers.saturating_sub(1);
                if (*m).writers == 0 { (*m).read_wait.wake_up(); }
                // Free pipe when both ends closed
                if (*m).readers == 0 && (*m).writers == 0 {
                    free_page(page);
                    PIPE_PAGES[idx] = 0;
                }
            }
        }
    }
}

fn buf_offset() -> usize { core::mem::size_of::<PipeMeta>() }

pub fn pipe_read(idx: usize, buf: *mut u8, count: usize) -> i64 {
    if buf.is_null() || count == 0 { return 0; }
    unsafe {
        let page = PIPE_PAGES[idx];
        if page == 0 { return 0; }
        let m = meta(page);
        let ring = (page + buf_offset()) as *const u8;
        let mut total: i64 = 0;
        loop {
            if (*m).len > 0 {
                let n = core::cmp::min(count - total as usize, (*m).len);
                let first = core::cmp::min(n, PIPE_BUF_SIZE - (*m).read_pos);
                core::ptr::copy_nonoverlapping(
                    ring.add((*m).read_pos), buf.add(total as usize), first);
                (*m).read_pos = ((*m).read_pos + first) % PIPE_BUF_SIZE;
                (*m).len -= first;
                total += first as i64;
                if first < n {
                    let second = n - first;
                    core::ptr::copy_nonoverlapping(ring, buf.add(total as usize), second);
                    (*m).read_pos = second;
                    (*m).len -= second;
                    total += second as i64;
                }
                (*m).write_wait.wake_up();
                return total;
            }
            if (*m).writers == 0 { return total; }
            if total > 0 { return total; }
            (*m).read_wait.sleep_on();
        }
    }
}

pub fn pipe_write(idx: usize, buf: *const u8, count: usize) -> i64 {
    if buf.is_null() || count == 0 { return 0; }
    unsafe {
        let page = PIPE_PAGES[idx];
        if page == 0 { return 0; }
        let m = meta(page);
        if (*m).readers == 0 { return -(EPIPE as i64); }
        let ring = (page + buf_offset()) as *mut u8;
        let mut total: i64 = 0;
        loop {
            if (*m).readers == 0 { return -(EPIPE as i64); }
            let free = PIPE_BUF_SIZE - (*m).len;
            if free > 0 {
                let n = core::cmp::min(count - total as usize, free);
                let first = core::cmp::min(n, PIPE_BUF_SIZE - (*m).write_pos);
                core::ptr::copy_nonoverlapping(
                    buf.add(total as usize), ring.add((*m).write_pos), first);
                (*m).write_pos = ((*m).write_pos + first) % PIPE_BUF_SIZE;
                (*m).len += first;
                total += first as i64;
                if first < n {
                    let second = n - first;
                    core::ptr::copy_nonoverlapping(
                        buf.add(total as usize), ring, second);
                    (*m).write_pos = second;
                    (*m).len += second;
                    total += second as i64;
                }
                (*m).read_wait.wake_up();
                if total as usize >= count { return total; }
            }
            if (*m).readers == 0 { return -(EPIPE as i64); }
            (*m).write_wait.sleep_on();
        }
    }
}
