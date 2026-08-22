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
/// 每任务管道 fd 表：fd → 管道下标。NIL = 不是管道 fd。
/// 和普通 fd 表（TASK_FILP）一样是 per-task 的，否则 fork/exec 会互相踩。
static mut PIPE_FD_MAP: [[usize; MAX_PIPE_FD]; crate::sched::NR_TASKS] =
    [[NIL; MAX_PIPE_FD]; crate::sched::NR_TASKS];
/// 每个管道 fd 的方向：0 = 读端，1 = 写端。
static mut PIPE_FD_DIR: [[u8; MAX_PIPE_FD]; crate::sched::NR_TASKS] =
    [[0; MAX_PIPE_FD]; crate::sched::NR_TASKS];

pub const PIPE_DIR_READ: u8 = 0;
pub const PIPE_DIR_WRITE: u8 = 1;

unsafe fn meta(page: usize) -> *mut PipeMeta {
    page as *mut PipeMeta
}

/// 当前任务下标（fd 表都是 per-task 的）。
fn cur() -> usize { crate::sched::current_index() }

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

pub fn register_fd(fd: usize, pipe_idx: usize, dir: u8) {
    unsafe {
        if fd < MAX_PIPE_FD {
            let c = cur();
            PIPE_FD_MAP[c][fd] = pipe_idx;
            PIPE_FD_DIR[c][fd] = dir;
        }
    }
}

pub fn unregister_fd(fd: usize) {
    unsafe {
        if fd < MAX_PIPE_FD { PIPE_FD_MAP[cur()][fd] = NIL; }
    }
}

pub fn fd_is_pipe(fd: usize) -> bool {
    unsafe { fd < MAX_PIPE_FD && PIPE_FD_MAP[cur()][fd] != NIL }
}

pub fn fd_to_pipe(fd: usize) -> Option<usize> {
    unsafe {
        if fd < MAX_PIPE_FD && PIPE_FD_MAP[cur()][fd] != NIL {
            Some(PIPE_FD_MAP[cur()][fd])
        } else { None }
    }
}

/// 管道 fd 的方向。fd 必须是管道 fd。
pub fn fd_dir(fd: usize) -> u8 {
    unsafe { if fd < MAX_PIPE_FD { PIPE_FD_DIR[cur()][fd] } else { PIPE_DIR_READ } }
}

/// select/poll 的就绪状态查询。返回 (readable, writable, hangup)。
///
/// 对应原版 `pipe_select()`：读端在「有数据」或「写端全部关闭（EOF）」时
/// 可读；写端在「环缓冲未满」时可写，读端全部关闭时写会 EPIPE，poll 语义
/// 报 hangup。非管道 fd 不应调用（调用方先 `fd_is_pipe`）。
pub fn fd_poll_status(fd: usize) -> (bool, bool, bool) {
    unsafe {
        let idx = match fd_to_pipe(fd) { Some(i) => i, None => return (false, false, false) };
        let page = PIPE_PAGES[idx];
        if page == 0 { return (false, false, true); }
        let m = meta(page);
        match fd_dir(fd) {
            PIPE_DIR_READ => {
                let readable = (*m).len > 0 || (*m).writers == 0;
                (readable, false, false)
            }
            _ => {
                if (*m).readers == 0 {
                    (false, false, true) // 写会得到 EPIPE
                } else {
                    (false, (*m).len < ring_size(), false)
                }
            }
        }
    }
}

/// 复制一个管道 fd（dup / dup2 / fcntl F_DUPFD 用）：
/// `newfd` 指向同一个管道的同一端，并给对应端加一次引用计数。
/// 返回 false 表示 oldfd 不是管道 fd。
pub fn dup_fd(oldfd: usize, newfd: usize) -> bool {
    if let Some(idx) = fd_to_pipe(oldfd) {
        let dir = fd_dir(oldfd);
        unsafe {
            register_fd(newfd, idx, dir);
            let page = PIPE_PAGES[idx];
            if page != 0 {
                let m = meta(page);
                if dir == PIPE_DIR_READ {
                    (*m).readers = (*m).readers.saturating_add(1);
                } else {
                    (*m).writers = (*m).writers.saturating_add(1);
                }
            }
        }
        true
    } else {
        false
    }
}

/// 关闭一个管道 fd：按方向递减对应端计数；读写两端都归零时释放管道页。
pub fn close_fd(fd: usize) {
    if let Some(idx) = fd_to_pipe(fd) {
        let dir = fd_dir(fd);
        unsafe {
            let page = PIPE_PAGES[idx];
            if page != 0 {
                let m = meta(page);
                if dir == PIPE_DIR_READ {
                    (*m).readers = (*m).readers.saturating_sub(1);
                    if (*m).readers == 0 { (*m).write_wait.wake_up(); }
                } else {
                    (*m).writers = (*m).writers.saturating_sub(1);
                    if (*m).writers == 0 { (*m).read_wait.wake_up(); }
                }
                if (*m).readers == 0 && (*m).writers == 0 {
                    free_page(page);
                    PIPE_PAGES[idx] = 0;
                }
            }
        }
        unregister_fd(fd);
    }
}

/// fork 时复制管道 fd 表，并给每个被复制的 fd 对应端加一次引用计数。
/// 对应普通 fd 表的 [`crate::fs::open::clone_fds`]。
pub fn clone_pipe_fds(from: usize, to: usize) {
    if from >= crate::sched::NR_TASKS || to >= crate::sched::NR_TASKS || from == to {
        return;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(&raw const PIPE_FD_MAP[from], &raw mut PIPE_FD_MAP[to], 1);
        core::ptr::copy_nonoverlapping(&raw const PIPE_FD_DIR[from], &raw mut PIPE_FD_DIR[to], 1);
        // 每个被复制的 fd 给对应端加一次引用计数。
        for fd in 0..MAX_PIPE_FD {
            let idx = PIPE_FD_MAP[to][fd];
            if idx == NIL { continue; }
            let dir = PIPE_FD_DIR[to][fd];
            let page = PIPE_PAGES[idx];
            if page != 0 {
                let m = meta(page);
                if dir == PIPE_DIR_READ { (*m).readers += 1; }
                else { (*m).writers += 1; }
            }
        }
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

/// 环缓冲实际可用字节数。PipeMeta 占页首 `buf_offset()` 字节，
/// 环缓冲只能用剩下的 `PIPE_BUF_SIZE - buf_offset()` 字节；之前按
/// `PIPE_BUF_SIZE`(4096) 计会让 write_pos/read_pos 到 4048+ 时越界写 48 字节
/// 到下一页，踩坏相邻页（潜在内存损坏）。
fn ring_size() -> usize { PIPE_BUF_SIZE - buf_offset() }

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
                let first = core::cmp::min(n, ring_size() - (*m).read_pos);
                // 走 copy_to_user（translate 逐页 + 惰性解析），不要直接解引用用户
                // 地址：直接 copy_nonoverlapping 会读内核恒等映射而不是用户页表。
                let pml4 = (*crate::sched::task_ptr(crate::sched::current_index())).pml4;
                let r1 = crate::mm::area::copy_to_user(buf as u64 + total as u64, ring.add((*m).read_pos), first, pml4);
                if r1 < 0 { return r1; }
                (*m).read_pos = ((*m).read_pos + first) % ring_size();
                (*m).len -= first;
                total += first as i64;
                if first < n {
                    let second = n - first;
                    let r2 = crate::mm::area::copy_to_user(buf as u64 + total as u64, ring, second, pml4);
                    if r2 < 0 { return r2; }
                    (*m).read_pos = second;
                    (*m).len -= second;
                    total += second as i64;
                }
                (*m).write_wait.wake_up();
                return total;
            }
            if (*m).writers == 0 { return total; }
            if total > 0 { return total; }
            // 用 sleep_on_while 而非 sleep_on：先挂队列、再在关中断下复查条件。
            // 否则「判完 writers!=0 到挂上队列」之间写端关闭、wake_up 落空，
            // 读者永久睡死（bash 命令替换的 EOF 读就死在这）。
            let len_p = core::ptr::addr_of!((*m).len);
            let writers_p = core::ptr::addr_of!((*m).writers);
            // SAFETY: m 是有效管道页指针；sleep_on_while 契约要求进程上下文。
            unsafe {
                (*m).read_wait.sleep_on_while(|| {
                    core::ptr::read_volatile(len_p) == 0 && core::ptr::read_volatile(writers_p) != 0
                })
            }
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
            let free = ring_size() - (*m).len;
            if free > 0 {
                let n = core::cmp::min(count - total as usize, free);
                let first = core::cmp::min(n, ring_size() - (*m).write_pos);
                // 走 copy_from_user（translate 逐页 + 惰性解析），避免直接解引用用户地址。
                let pml4 = (*crate::sched::task_ptr(crate::sched::current_index())).pml4;
                let r1 = crate::mm::area::copy_from_user(ring.add((*m).write_pos), buf as u64 + total as u64, first, pml4);
                if r1 < 0 { return r1; }
                (*m).write_pos = ((*m).write_pos + first) % ring_size();
                (*m).len += first;
                total += first as i64;
                if first < n {
                    let second = n - first;
                    let r2 = crate::mm::area::copy_from_user(ring, buf as u64 + total as u64, second, pml4);
                    if r2 < 0 { return r2; }
                    (*m).write_pos = second;
                    (*m).len += second;
                    total += second as i64;
                }
                (*m).read_wait.wake_up();
                if total as usize >= count { return total; }
            }
            if (*m).readers == 0 { return -(EPIPE as i64); }
            // 同 pipe_read：用 sleep_on_while 消除丢失唤醒窗口。
            let len_p = core::ptr::addr_of!((*m).len);
            let readers_p = core::ptr::addr_of!((*m).readers);
            // SAFETY: m 有效；sleep_on_while 契约要求进程上下文。
            unsafe {
                (*m).write_wait.sleep_on_while(|| {
                    core::ptr::read_volatile(len_p) >= ring_size()
                        && core::ptr::read_volatile(readers_p) != 0
                })
            }
        }
    }
}

// =============================================================================
// splice / tee / vmsplice 用的内核缓冲管道操作（不经 copy_to/from_user）
// =============================================================================

/// 从管道环缓冲读入**内核**缓冲区（`dst` 是内核地址，不经 copy_to_user）。
/// 非阻塞：空则返回 0。
pub fn pipe_read_kernel(idx: usize, dst: *mut u8, count: usize) -> i64 {
    if dst.is_null() || count == 0 { return 0; }
    // SAFETY: dst 是调用方分配的内核页；idx 有效。
    unsafe {
        let page = PIPE_PAGES[idx];
        if page == 0 { return 0; }
        let m = meta(page);
        if (*m).len == 0 { return 0; }
        let ring = (page + buf_offset()) as *const u8;
        let n = core::cmp::min(count, (*m).len);
        let first = core::cmp::min(n, ring_size() - (*m).read_pos);
        core::ptr::copy_nonoverlapping(ring.add((*m).read_pos), dst, first);
        (*m).read_pos = ((*m).read_pos + first) % ring_size();
        (*m).len -= first;
        let mut total = first;
        if first < n {
            let second = n - first;
            core::ptr::copy_nonoverlapping(ring, dst.add(first), second);
            (*m).read_pos = second;
            (*m).len -= second;
            total += second;
        }
        (*m).write_wait.wake_up();
        total as i64
    }
}

/// 把**内核**缓冲区写入管道环缓冲（`src` 是内核地址，不经 copy_from_user）。
/// 非阻塞：满则返回 0。
pub fn pipe_write_kernel(idx: usize, src: *const u8, count: usize) -> i64 {
    if src.is_null() || count == 0 { return 0; }
    // SAFETY: src 是调用方分配的内核页；idx 有效。
    unsafe {
        let page = PIPE_PAGES[idx];
        if page == 0 { return 0; }
        let m = meta(page);
        if (*m).readers == 0 { return -(EPIPE as i64); }
        let free = ring_size() - (*m).len;
        if free == 0 { return 0; }
        let ring = (page + buf_offset()) as *mut u8;
        let n = core::cmp::min(count, free);
        let first = core::cmp::min(n, ring_size() - (*m).write_pos);
        core::ptr::copy_nonoverlapping(src, ring.add((*m).write_pos), first);
        (*m).write_pos = ((*m).write_pos + first) % ring_size();
        (*m).len += first;
        let mut total = first;
        if first < n {
            let second = n - first;
            core::ptr::copy_nonoverlapping(src.add(first), ring, second);
            (*m).write_pos = second;
            (*m).len += second;
            total += second;
        }
        (*m).read_wait.wake_up();
        total as i64
    }
}

/// 从管道环缓冲**只读不消费**地读入内核缓冲区（tee 用）。非阻塞：空返回 0。
pub fn pipe_peek_kernel(idx: usize, dst: *mut u8, count: usize) -> i64 {
    if dst.is_null() || count == 0 { return 0; }
    // SAFETY: dst 是内核页；idx 有效。
    unsafe {
        let page = PIPE_PAGES[idx];
        if page == 0 { return 0; }
        let m = meta(page);
        if (*m).len == 0 { return 0; }
        let ring = (page + buf_offset()) as *const u8;
        let n = core::cmp::min(count, (*m).len);
        let first = core::cmp::min(n, ring_size() - (*m).read_pos);
        core::ptr::copy_nonoverlapping(ring.add((*m).read_pos), dst, first);
        if first < n {
            core::ptr::copy_nonoverlapping(ring, dst.add(first), n - first);
        }
        n as i64
    }
}
