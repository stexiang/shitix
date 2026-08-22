//! 匿名事件 fd：`eventfd` / `timerfd` / `signalfd` / `epoll` / `inotify` 的
//! 公共基础设施。对应 Linux 的 `fs/eventfd.c`、`fs/timerfd.c`、
//! `fs/signalfd.c`、`fs/eventpoll.c`、`fs/notify/inotify/*`。
//!
//! 与 [`super::pipe`] 同构：对象放在静态槽位数组里，fd → 对象 的映射是
//! **per-task** 的旁路数组（`FD_MAP`），不进 BSS 大户、也不走 VFS inode。
//!
//! # 与原版的差异
//! 1. **阻塞靠让出循环**。读/写在「会阻塞」且非 O_NONBLOCK 时
//!    `schedule()` 让出重试（合作式调度的 waitqueue 等价物）；
//!    O_NONBLOCK 时返回 `-EAGAIN`。
//! 2. timerfd 的时钟是 jiffies(100Hz)，不支持 interval 以外的精度。

use crate::klib::errno::{EAGAIN, EBADF, EINVAL};

/// fd → 对象下标的映射表宽度。fd 0..=31。
pub const MAX_EV_FD: usize = 32;
/// 匿名事件 fd 对象槽位数。
const MAX_OBJS: usize = 16;
/// 无效槽位。
const NIL: usize = usize::MAX;

/// 对象类型。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EvKind {
    /// eventfd：一个 u64 计数器，写累加、读清零。
    Event,
    /// timerfd：到期后可读，读返回到期次数。
    Timer,
    /// signalfd：读返回待处理信号。
    Signal,
    /// epoll：就绪事件多路复用。
    Epoll,
    /// inotify：文件系统事件通知。
    Inotify,
}

/// 一个匿名事件 fd 对象。
#[derive(Clone, Copy)]
pub struct EvObj {
    pub kind: EvKind,
    /// eventfd 计数器 / timerfd 累计到期次数 / signalfd 未使用。
    pub val: u64,
    /// timerfd 下次到期 jiffies（0 = 未激活）。
    pub deadline: u64,
    /// timerfd 周期（jiffies；0 = 一次性）。
    pub interval: u64,
    /// signalfd 的 sigset 掩码（位号 == 信号号，见 signal.rs 约定）。
    pub sigmask: u64,
    /// 创建时是否带 O_NONBLOCK。
    pub nonblock: bool,
}

impl EvObj {
    const fn empty() -> Self {
        EvObj {
            kind: EvKind::Event,
            val: 0,
            deadline: 0,
            interval: 0,
            sigmask: 0,
            nonblock: false,
        }
    }
}

static mut OBJS: [EvObj; MAX_OBJS] = [const { EvObj::empty() }; MAX_OBJS];
/// 每任务 fd 映射：`FD_MAP[task][fd]` = 对象下标 + 1；0 = 非事件 fd
/// （全 0 初始化落在 BSS；NIL=0xFF.. 编码会把 16KB 烧进镜像）。
static mut FD_MAP: [[usize; MAX_EV_FD]; crate::sched::NR_TASKS] =
    [[0; MAX_EV_FD]; crate::sched::NR_TASKS];

fn cur() -> usize {
    crate::sched::current_index()
}

/// 分配一个对象槽位并初始化。
pub fn alloc(kind: EvKind, nonblock: bool) -> Option<usize> {
    // SAFETY: 系统调用上下文，单核。
    unsafe {
        for i in 0..MAX_OBJS {
            if !slot_used(i) {
                let p = core::ptr::addr_of_mut!(OBJS[i]);
                (*p).kind = kind;
                (*p).nonblock = nonblock;
                (*p).val = 0;
                (*p).deadline = 0;
                (*p).interval = 0;
                (*p).sigmask = 0;
                return Some(i);
            }
        }
    }
    None
}

/// 某个对象槽位是否还被任何任务的 fd 引用。
fn slot_used(idx: usize) -> bool {
    // SAFETY: 只读 FD_MAP；系统调用上下文。
    unsafe {
        for t in 0..crate::sched::NR_TASKS {
            for fd in 0..MAX_EV_FD {
                if FD_MAP[t][fd] == idx + 1 {
                    return true;
                }
            }
        }
    }
    false
}

/// 释放对象槽位（close 时调用）。
fn free_obj(idx: usize) {
    if idx < MAX_OBJS {
        // SAFETY: 系统调用上下文。
        unsafe { *core::ptr::addr_of_mut!(OBJS[idx]) = EvObj::empty(); }
    }
}

/// 把 fd 注册到对象下标。
pub fn register_fd(fd: usize, obj_idx: usize) {
    if fd < MAX_EV_FD {
        // SAFETY: 系统调用上下文，单核。
        unsafe { FD_MAP[cur()][fd] = obj_idx + 1; }
    }
}

/// 解除 fd 注册（close 时调用）。
pub fn unregister_fd(fd: usize) {
    if fd < MAX_EV_FD {
        // SAFETY: 系统调用上下文。
        unsafe { FD_MAP[cur()][fd] = 0; }
    }
}

/// 该 fd 是否匿名事件 fd。
pub fn fd_is_event(fd: usize) -> bool {
    // SAFETY: 只读；系统调用上下文。
    unsafe { fd < MAX_EV_FD && FD_MAP[cur()][fd] != 0 }
}

/// 取 fd 对应的对象下标。
pub fn fd_to_obj(fd: usize) -> Option<usize> {
    // SAFETY: 只读；系统调用上下文。
    unsafe {
        if fd < MAX_EV_FD && FD_MAP[cur()][fd] != 0 {
            Some(FD_MAP[cur()][fd] - 1)
        } else {
            None
        }
    }
}

fn obj(idx: usize) -> *mut EvObj {
    debug_assert!(idx < MAX_OBJS);
    // SAFETY: idx 由调用方保证在界内。
    unsafe { core::ptr::addr_of_mut!(OBJS[idx]) }
}

/// 关闭一个事件 fd：解除映射；若对象不再被引用则释放。
pub fn close(fd: usize) {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return,
    };
    unregister_fd(fd);
    if !slot_used(idx) {
        free_obj(idx);
    }
}

// =============================================================================
// eventfd
// =============================================================================

/// 创建 eventfd。`initval` 是初始计数，`flags` 支持 `EFD_NONBLOCK`(0x800)、
/// `EFD_CLOEXEC`(0x80000，忽略)。返回 fd。
pub fn eventfd_create(initval: u32, flags: u32) -> i64 {
    let nonblock = flags & 0x800 != 0;
    let idx = match alloc(EvKind::Event, nonblock) {
        Some(i) => i,
        None => return -(crate::klib::errno::EMFILE as i64),
    };
    // SAFETY: idx 刚分配。
    unsafe {
        (*obj(idx)).val = initval as u64;
    }
    idx as i64
}

/// 把对象下标绑定到一个新 fd。`fd_hint` 是「从 3 开始找空闲 fd」的调用约定，
/// 实际分配逻辑在 sys.rs 的 `alloc_event_fd`。
pub fn bind_fd(obj_idx: usize, fd: usize) {
    register_fd(fd, obj_idx);
}

/// eventfd 读：返回计数并清零。计数为 0 → -EAGAIN。
pub fn eventfd_read(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    if len < 8 {
        return -(EINVAL as i64);
    }
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    let p = obj(idx);
    let v = unsafe { (*p).val };
    if v == 0 {
        return -(EAGAIN as i64);
    }
    unsafe { (*p).val = 0; }
    // SAFETY: 调用方已校验 buf_ptr 可写（sys.rs 层做 user_ok）。
    unsafe { core::ptr::write_unaligned(buf_ptr as *mut u64, v) };
    8
}

/// eventfd 写：累加计数。溢出或全 1 → -EAGAIN。
pub fn eventfd_write(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    if len < 8 {
        return -(EINVAL as i64);
    }
    // SAFETY: 调用方已校验 buf_ptr 可读。
    let add = unsafe { core::ptr::read_unaligned(buf_ptr as *const u64) };
    // 全 1（0xffff_ffff_ffff_ffff）是「宁可失败」的哨兵值。
    if add == u64::MAX {
        return -(EAGAIN as i64);
    }
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    let p = obj(idx);
    let cur = unsafe { (*p).val };
    // 上限 u64::MAX - 1，否则 -EAGAIN（原版 EFD 语义）。
    if add > (u64::MAX - 1) - cur {
        return -(EAGAIN as i64);
    }
    unsafe { (*p).val = cur + add; }
    8
}

// =============================================================================
// timerfd
// =============================================================================

/// 创建 timerfd。clockid 只接受 CLOCK_REALTIME(0)/CLOCK_MONOTONIC(1)/
/// 其余忽略。返回 fd。
pub fn timerfd_create(clockid: u32, flags: u32) -> i64 {
    let _ = clockid;
    let nonblock = flags & 0x800 != 0;
    let idx = match alloc(EvKind::Timer, nonblock) {
        Some(i) => i,
        None => return -(crate::klib::errno::EMFILE as i64),
    };
    idx as i64
}

/// `struct itimerspec`（两个 timespec）。
#[repr(C)]
pub struct ItimerSpec {
    pub it_interval_sec: i64,
    pub it_interval_nsec: i64,
    pub it_value_sec: i64,
    pub it_value_nsec: i64,
}

/// 秒+纳秒 → jiffies（向上取整，至少 1 tick）。
fn to_jiffies(sec: i64, nsec: i64) -> u64 {
    if sec < 0 || nsec < 0 {
        return 0;
    }
    let hz = crate::sched::HZ;
    let ticks = (sec as u64) * hz + (nsec as u64) / (1_000_000_000 / hz);
    if ticks == 0 {
        1
    } else {
        ticks
    }
}

/// timerfd 设置：读 itimerspec，设置 deadline/interval。返回 0。
pub fn timerfd_settime(fd: usize, _flags: u32, spec_ptr: u64, _old_ptr: u64) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: 调用方已校验 spec_ptr 可读。
    let spec = unsafe { core::ptr::read_unaligned(spec_ptr as *const ItimerSpec) };
    let interval = to_jiffies(spec.it_interval_sec, spec.it_interval_nsec);
    let value = to_jiffies(spec.it_value_sec, spec.it_value_nsec);
    // SAFETY: idx 有效。
    let p = obj(idx);
    let now = crate::sched::jiffies();
    unsafe {
        (*p).interval = interval;
        (*p).deadline = if value == 0 { 0 } else { now + value };
        (*p).val = 0; // 重置累计到期次数
    }
    0
}

/// timerfd 取当前值。返回 0，写回剩余时间与周期。
pub fn timerfd_gettime(fd: usize, spec_ptr: u64) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    let p = obj(idx);
    let now = crate::sched::jiffies();
    let (rem, interval) = unsafe {
        let d = (*p).deadline;
        let iv = (*p).interval;
        if d == 0 {
            (0u64, iv)
        } else if d > now {
            (d - now, iv)
        } else {
            (0, iv)
        }
    };
    // 写回 itimerspec。
    let hz = crate::sched::HZ;
    let spec = ItimerSpec {
        it_interval_sec: (interval / hz) as i64,
        it_interval_nsec: ((interval % hz) * (1_000_000_000 / hz)) as i64,
        it_value_sec: (rem / hz) as i64,
        it_value_nsec: ((rem % hz) * (1_000_000_000 / hz)) as i64,
    };
    // SAFETY: 调用方已校验 spec_ptr 可写。
    unsafe { core::ptr::write_unaligned(spec_ptr as *mut ItimerSpec, spec) };
    0
}

/// timerfd 读：返回累计到期次数。未到期 → -EAGAIN。
pub fn timerfd_read(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    if len < 8 {
        return -(EINVAL as i64);
    }
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    let p = obj(idx);
    let now = crate::sched::jiffies();
    let (exp, next_deadline) = unsafe {
        let d = (*p).deadline;
        if d == 0 || d > now {
            return -(EAGAIN as i64);
        }
        let iv = (*p).interval;
        // 到期次数 = (now - d) / iv + 1（iv==0 时一次）。
        let exp = if iv == 0 {
            1
        } else {
            (now - d) / iv + 1
        };
        let next = if iv == 0 {
            0
        } else {
            // 推进到下一次未到的到期点。
            d + exp * iv
        };
        (exp, next)
    };
    unsafe {
        (*p).deadline = next_deadline;
        (*p).val += exp;
    }
    // SAFETY: 调用方已校验 buf_ptr 可写。
    unsafe { core::ptr::write_unaligned(buf_ptr as *mut u64, exp) };
    8
}

// =============================================================================
// signalfd
// =============================================================================

/// 创建 signalfd（分配对象）。返回对象下标，负数为 errno。
pub fn signalfd_alloc(flags: u32) -> i64 {
    let nonblock = flags & 0x800 != 0;
    match alloc(EvKind::Signal, nonblock) {
        Some(i) => i as i64,
        None => -(crate::klib::errno::EMFILE as i64),
    }
}

/// 更新已有 signalfd 的信号掩码。
pub fn signalfd_set_mask(fd: usize, mask: u64) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    unsafe {
        (*obj(idx)).kind = EvKind::Signal;
        (*obj(idx)).sigmask = mask;
    }
    0
}

/// `struct signalfd_siginfo`（x86_64，128 字节）。
#[repr(C)]
pub struct SignalfdSiginfo {
    pub ssi_signo: u32,
    pub ssi_errno: i32,
    pub ssi_code: i32,
    pub ssi_pid: u32,
    pub ssi_uid: u32,
    pub ssi_fd: i32,
    pub ssi_tid: u32,
    pub ssi_band: u32,
    pub ssi_overrun: u32,
    pub ssi_trapno: u32,
    pub ssi_status: i32,
    pub ssi_int: i32,
    pub ssi_ptr: u64,
    pub ssi_utime: u64,
    pub ssi_stime: u64,
    pub ssi_addr: u64,
    pub ssi_addr_lsb: u16,
    pub __pad0: [u8; 2],
    pub ssi_syscall: i32,
    pub ssi_call_addr: u64,
    pub ssi_arch: u32,
    pub __pad: [u8; 28],
}

/// signalfd 读：返回一个待处理信号。无待处理信号 → -EAGAIN。
pub fn signalfd_read(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    if len < core::mem::size_of::<SignalfdSiginfo>() as u64 {
        return -(EINVAL as i64);
    }
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    let mask = unsafe { (*obj(idx)).sigmask };
    // 从当前任务的待处理信号里挑一个掩码内的最低位。
    // SAFETY: 只读 current。
    let pending = unsafe { (*crate::sched::task_ptr(cur())).signal } & mask;
    if pending == 0 {
        return -(EAGAIN as i64);
    }
    let signo = pending.trailing_zeros() as u32;
    // 消费该信号位（对齐原版：读走即清除）。
    // SAFETY: 系统调用上下文，单核。
    unsafe {
        let tp = crate::sched::task_ptr(cur());
        (*tp).signal &= !(1u64 << signo);
    }
    let mut si = SignalfdSiginfo {
        ssi_signo: signo,
        ssi_errno: 0,
        ssi_code: 0,
        ssi_pid: 0,
        ssi_uid: 0,
        ssi_fd: 0,
        ssi_tid: 0,
        ssi_band: 0,
        ssi_overrun: 0,
        ssi_trapno: 0,
        ssi_status: 0,
        ssi_int: 0,
        ssi_ptr: 0,
        ssi_utime: 0,
        ssi_stime: 0,
        ssi_addr: 0,
        ssi_addr_lsb: 0,
        __pad0: [0; 2],
        ssi_syscall: 0,
        ssi_call_addr: 0,
        ssi_arch: 0,
        __pad: [0; 28],
    };
    si.ssi_pid = unsafe { (*crate::sched::task_ptr(cur())).pid as u32 };
    // SAFETY: 调用方已校验 buf_ptr 可写。
    unsafe { core::ptr::write_unaligned(buf_ptr as *mut SignalfdSiginfo, si) };
    core::mem::size_of::<SignalfdSiginfo>() as i64
}

// =============================================================================
// epoll
// =============================================================================

/// epoll 监视条目（每 fd 一个）。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

/// epoll 创建。返回 fd。
pub fn epoll_create(flags: u32) -> i64 {
    let nonblock = flags & 0x800 != 0;
    match alloc(EvKind::Epoll, nonblock) {
        Some(i) => i as i64,
        None => -(crate::klib::errno::EMFILE as i64),
    }
}

/// 每个 epoll 对象最多监听的 fd 数。
const MAX_EPOLL_INTEREST: usize = 16;

/// 一条 epoll 监听项。
#[derive(Clone, Copy)]
struct EpollInterest {
    used: bool,
    fd: usize,
    events: u32,
    data: u64,
}

const EMPTY_INTEREST: EpollInterest = EpollInterest { used: false, fd: 0, events: 0, data: 0 };

/// epoll 兴趣表（按下标挂在对应对象槽位上）。
static mut EPOLL_TAB: [[EpollInterest; MAX_EPOLL_INTEREST]; MAX_OBJS] =
    [[EMPTY_INTEREST; MAX_EPOLL_INTEREST]; MAX_OBJS];

/// EPOLL_CTL_ADD/MOD/DEL
const EPOLL_CTL_ADD: u32 = 1;
const EPOLL_CTL_MOD: u32 = 3;
const EPOLL_CTL_DEL: u32 = 2;
/// 事件位
const EPOLLIN: u32 = 0x1;
const EPOLLOUT: u32 = 0x4;
const EPOLLERR: u32 = 0x8;
const EPOLLHUP: u32 = 0x10;

/// epoll_ctl：add/mod/del。真实维护兴趣表（原版 eventpoll.c 的红黑树
/// 这里摊平成定长数组）。
pub fn epoll_ctl(epfd: usize, op: u32, fd: usize, ev_ptr: u64) -> i64 {
    let idx = match fd_to_obj(epfd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    if unsafe { (*obj(idx)).kind } != EvKind::Epoll {
        return -(EINVAL as i64);
    }
    if fd >= MAX_EV_FD {
        return -(EBADF as i64);
    }
    // SAFETY: 系统调用上下文独占兴趣表。
    let tab = unsafe { &mut *core::ptr::addr_of_mut!(EPOLL_TAB[idx]) };
    let pos = tab.iter().position(|e| e.used && e.fd == fd);
    match op {
        EPOLL_CTL_ADD => {
            if pos.is_some() {
                return -(crate::klib::errno::EEXIST as i64);
            }
            if ev_ptr == 0 {
                return -(EINVAL as i64);
            }
            // SAFETY: 用户指针读 epoll_event。
            let (events, data) = unsafe {
                (core::ptr::read_volatile(ev_ptr as *const u32),
                 core::ptr::read_volatile((ev_ptr + 8) as *const u64))
            };
            match tab.iter().position(|e| !e.used) {
                None => -(crate::klib::errno::ENOSPC as i64),
                Some(slot) => {
                    tab[slot] = EpollInterest { used: true, fd, events, data };
                    0
                }
            }
        }
        EPOLL_CTL_MOD => {
            let slot = match pos {
                None => return -(crate::klib::errno::ENOENT as i64),
                Some(s) => s,
            };
            if ev_ptr == 0 {
                return -(EINVAL as i64);
            }
            // SAFETY: 用户指针读 epoll_event。
            unsafe {
                tab[slot].events = core::ptr::read_volatile(ev_ptr as *const u32);
                tab[slot].data = core::ptr::read_volatile((ev_ptr + 8) as *const u64);
            }
            0
        }
        EPOLL_CTL_DEL => {
            match pos {
                None => -(crate::klib::errno::ENOENT as i64),
                Some(slot) => {
                    tab[slot] = EMPTY_INTEREST;
                    0
                }
            }
        }
        _ => -(EINVAL as i64),
    }
}

/// 查一个 fd 当前的 (readable, writable, hangup)。
fn fd_ready(fd: usize) -> (bool, bool, bool) {
    if crate::fs::pipe::fd_is_pipe(fd) {
        return crate::fs::pipe::fd_poll_status(fd);
    }
    if fd_is_event(fd) {
        // 事件 fd：eventfd 计数>0、timerfd 已到期、signalfd 有待处理信号
        // SAFETY: fd_to_obj 已确认映射存在。
        if let Some(i) = fd_to_obj(fd) {
            let now = crate::sched::jiffies();
            // SAFETY: i 有效。
            unsafe {
                let p = obj(i);
                let readable = match (*p).kind {
                    EvKind::Event => (*p).val > 0,
                    EvKind::Timer => (*p).deadline != 0 && (*p).deadline <= now,
                    EvKind::Signal => {
                        let t = crate::sched::task_ptr(crate::sched::current_index());
                        (*t).signal & (*p).sigmask != 0
                    }
                    EvKind::Inotify => inotify_has_events(i),
                    EvKind::Epoll => false,
                };
                return (readable, true, false);
            }
        }
        return (false, false, false);
    }
    if crate::net::socket::fd_is_socket(fd) {
        // socket 层没有就绪查询；保守报可写
        return (false, true, false);
    }
    // 普通文件/字符设备：同 select/poll 的「存在即就绪」
    if unsafe { crate::fs::open::fd_to_filp(fd) } != crate::fs::inode::NIL {
        return (true, true, false);
    }
    (false, false, false)
}

/// 收集一轮就绪事件，返回写入的事件数。
fn epoll_collect(idx: usize, ev_ptr: u64, maxevents: u32) -> usize {
    // SAFETY: 系统调用上下文读兴趣表。
    let tab = unsafe { &*core::ptr::addr_of!(EPOLL_TAB[idx]) };
    let mut n = 0usize;
    for e in tab.iter() {
        if !e.used || n >= maxevents as usize {
            continue;
        }
        let (r, w, hup) = fd_ready(e.fd);
        let mut revents = 0u32;
        if r { revents |= EPOLLIN; }
        if w { revents |= EPOLLOUT; }
        if hup { revents |= EPOLLHUP; }
        // ERR/HUP 无条件上报，其余按兴趣掩码过滤
        revents &= e.events | EPOLLERR | EPOLLHUP;
        if revents == 0 {
            continue;
        }
        // SAFETY: 用户指针写 epoll_event（events@0 u32，data@8 u64）。
        unsafe {
            let dst = ev_ptr as usize + n * 16;
            core::ptr::write_volatile(dst as *mut u32, revents);
            core::ptr::write_volatile((dst + 8) as *mut u64, e.data);
        }
        n += 1;
    }
    n
}

/// epoll_wait：收集真实就绪事件；无事件且 timeout!=0 时睡到超时或
/// 出现就绪（合作式让出循环，同 read 的阻塞语义）。
pub fn epoll_wait(epfd: usize, ev_ptr: u64, maxevents: u32, timeout: i32) -> i64 {
    let idx = match fd_to_obj(epfd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    if unsafe { (*obj(idx)).kind } != EvKind::Epoll {
        return -(EINVAL as i64);
    }
    if maxevents == 0 || ev_ptr == 0 {
        return -(EINVAL as i64);
    }
    let hz = crate::sched::task::HZ;
    let deadline = if timeout < 0 {
        u64::MAX
    } else {
        crate::sched::jiffies()
            + (timeout as u64 * hz + 999) / 1000
    };
    loop {
        let n = epoll_collect(idx, ev_ptr, maxevents);
        if n > 0 {
            return n as i64;
        }
        if crate::sched::jiffies() >= deadline {
            return 0;
        }
        // SAFETY: 系统调用上下文让出。
        unsafe { crate::sched::schedule() };
    }
}

// =============================================================================
// inotify
// =============================================================================

/// inotify 初始化。返回 fd。
pub fn inotify_init(flags: u32) -> i64 {
    let nonblock = flags & 0x800 != 0;
    match alloc(EvKind::Inotify, nonblock) {
        Some(i) => i as i64,
        None => -(crate::klib::errno::EMFILE as i64),
    }
}

/// 每个 inotify 对象最多 watch 数。
const MAX_WATCHES: usize = 8;
/// watch 路径最大长度（含 NUL）。
const MAX_WATCH_PATH: usize = 128;
/// 事件名最大长度（含 NUL）。
const MAX_EV_NAME: usize = 64;
/// 每对象事件环容量。
const MAX_INOTIFY_EVENTS: usize = 16;

/// inotify 事件位（inotify.h）
pub const IN_ACCESS: u32 = 0x1;
pub const IN_MODIFY: u32 = 0x2;
pub const IN_CREATE: u32 = 0x100;
pub const IN_DELETE: u32 = 0x200;
pub const IN_MOVED_FROM: u32 = 0x40;
pub const IN_MOVED_TO: u32 = 0x80;
pub const IN_ISDIR: u32 = 0x4000_0000;

/// 一条 watch。
#[derive(Clone, Copy)]
struct Watch {
    used: bool,
    wd: u32,
    mask: u32,
    path: [u8; MAX_WATCH_PATH],
}

const EMPTY_WATCH: Watch = Watch { used: false, wd: 0, mask: 0, path: [0; MAX_WATCH_PATH] };

/// 一条事件（对应用户态 struct inotify_event 的可变长 name）。
#[derive(Clone, Copy)]
struct InotifyEvent {
    used: bool,
    wd: u32,
    mask: u32,
    cookie: u32,
    /// 文件名（目录内事件），无名字事件为空串
    name: [u8; MAX_EV_NAME],
}

const EMPTY_EVENT: InotifyEvent = InotifyEvent {
    used: false, wd: 0, mask: 0, cookie: 0, name: [0; MAX_EV_NAME],
};

/// watch 表与事件环，按对象下标挂。
static mut INOTIFY_WATCHES: [[Watch; MAX_WATCHES]; MAX_OBJS] =
    [[EMPTY_WATCH; MAX_WATCHES]; MAX_OBJS];
static mut INOTIFY_EVENTS: [[InotifyEvent; MAX_INOTIFY_EVENTS]; MAX_OBJS] =
    [[EMPTY_EVENT; MAX_INOTIFY_EVENTS]; MAX_OBJS];
static mut NEXT_WD: u32 = 1;

/// inotify_add_watch：真实登记（路径字符串匹配，原版是 inode 挂 watcher）。
/// 同一对象重复 watch 同一路径 → 更新掩码返回原 wd（原版语义）。
pub fn inotify_add_watch(fd: usize, path_ptr: u64, mask: u32) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    if unsafe { (*obj(idx)).kind } != EvKind::Inotify {
        return -(EINVAL as i64);
    }
    if path_ptr == 0 || mask == 0 {
        return -(EINVAL as i64);
    }
    // 读路径（NUL 结尾）
    let mut path = [0u8; MAX_WATCH_PATH];
    let mut ok = false;
    for i in 0..MAX_WATCH_PATH {
        // SAFETY: 用户指针逐字节读到 NUL。
        let c = unsafe { core::ptr::read_volatile((path_ptr as *const u8).add(i)) };
        path[i] = c;
        if c == 0 { ok = true; break; }
    }
    if !ok {
        return -(crate::klib::errno::ENAMETOOLONG as i64);
    }
    // SAFETY: 系统调用上下文独占 watch 表。
    unsafe {
        let watches = &mut (*core::ptr::addr_of_mut!(INOTIFY_WATCHES))[idx];
        if let Some(w) = watches.iter_mut().find(|w| w.used && w.path == path) {
            w.mask = mask;
            return w.wd as i64;
        }
        match watches.iter_mut().find(|w| !w.used) {
            None => -(crate::klib::errno::ENOSPC as i64),
            Some(w) => {
                let wd = *core::ptr::addr_of!(NEXT_WD);
                *core::ptr::addr_of_mut!(NEXT_WD) = wd + 1;
                *w = Watch { used: true, wd, mask, path };
                wd as i64
            }
        }
    }
}

/// inotify_rm_watch。
pub fn inotify_rm_watch(fd: usize, wd: u32) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: 系统调用上下文。
    unsafe {
        let watches = &mut (*core::ptr::addr_of_mut!(INOTIFY_WATCHES))[idx];
        match watches.iter_mut().find(|w| w.used && w.wd == wd) {
            None => -(EINVAL as i64),
            Some(w) => {
                *w = EMPTY_WATCH;
                0
            }
        }
    }
}

/// 文件系统事件钩子：VFS/namei 层在 create/delete/rename 时调用。
/// `dir` 是事件所在目录的绝对路径（NUL 结尾），`name` 是事件涉及的
/// 最后一级名字（可为空）。所有 watch 了 `dir` 且掩码覆盖 `mask` 的
/// inotify 对象都会收到一条事件。
pub fn inotify_notify(dir: &[u8], mask: u32, name: &[u8], cookie: u32) {
    // SAFETY: 系统调用上下文（VFS 路径），独占两张表。
    unsafe {
        for idx in 0..MAX_OBJS {
            let watches = &(*core::ptr::addr_of!(INOTIFY_WATCHES))[idx];
            let mut hit_wd = None;
            for w in watches.iter() {
                if !w.used || w.mask & mask == 0 {
                    continue;
                }
                // 目录路径全等比较（含 NUL 的前缀）
                let wlen = w.path.iter().position(|&c| c == 0).unwrap_or(MAX_WATCH_PATH);
                if dir.len() == wlen && &w.path[..wlen] == dir {
                    hit_wd = Some(w.wd);
                    break;
                }
            }
            let wd = match hit_wd {
                Some(w) => w,
                None => continue,
            };
            let events = &mut (*core::ptr::addr_of_mut!(INOTIFY_EVENTS))[idx];
            // 找空槽；满了就丢最老的（环形语义：覆盖 slot 0 方向）
            let slot = match events.iter().position(|e| !e.used) {
                Some(s) => s,
                None => 0,
            };
            let mut ev = EMPTY_EVENT;
            ev.used = true;
            ev.wd = wd;
            ev.mask = mask;
            ev.cookie = cookie;
            let n = name.len().min(MAX_EV_NAME - 1);
            ev.name[..n].copy_from_slice(&name[..n]);
            ev.name[n] = 0;
            events[slot] = ev;
        }
    }
}

/// 该 inotify 对象是否有积压事件（epoll 就绪查询用）。
pub fn inotify_has_events(idx: usize) -> bool {
    if idx >= MAX_OBJS { return false; }
    // SAFETY: 只读。
    unsafe {
        (*core::ptr::addr_of!(INOTIFY_EVENTS))[idx].iter().any(|e| e.used)
    }
}

/// inotify 读：把积压事件按 `struct inotify_event` 变长记录拷到用户缓冲。
/// 无事件返回 -EAGAIN（阻塞语义由 read 分发层处理）。
pub fn inotify_read(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: 系统调用上下文。
    unsafe {
        let events = &mut (*core::ptr::addr_of_mut!(INOTIFY_EVENTS))[idx];
        let mut off = 0usize;
        let buf = buf_ptr as usize;
        let cap = len as usize;
        for e in events.iter_mut() {
            if !e.used { continue; }
            let nlen = e.name.iter().position(|&c| c == 0).unwrap_or(0) + 1;
            // 记录头 16 字节 + 名字（向上对齐到 16 的倍数以利解析）
            let rec = 16 + ((nlen + 15) & !15);
            if off + rec > cap {
                if off == 0 {
                    return -(EINVAL as i64); // 缓冲连一条都放不下
                }
                break;
            }
            // SAFETY: 用户指针恒等映射可写；写 struct inotify_event
            // {wd i32, mask u32, cookie u32, len u32} + name。
            let base = buf + off;
            core::ptr::write_volatile(base as *mut i32, e.wd as i32);
            core::ptr::write_volatile((base + 4) as *mut u32, e.mask);
            core::ptr::write_volatile((base + 8) as *mut u32, e.cookie);
            core::ptr::write_volatile((base + 12) as *mut u32, nlen as u32);
            core::ptr::copy_nonoverlapping(e.name.as_ptr(), (base + 16) as *mut u8, nlen);
            off += rec;
            *e = EMPTY_EVENT;
        }
        if off == 0 {
            -(EAGAIN as i64)
        } else {
            off as i64
        }
    }
}

// =============================================================================
// 统一读/写入口（sys.rs 的 read/write 里按 kind 分发）
// =============================================================================

/// 读一个匿名事件 fd。按对象类型分发。
///
/// 阻塞语义（原版 eventfd_read/timerfd_read/signalfd_read 的 wait queue
/// 等价物）：结果 -EAGAIN 且对象不带 O_NONBLOCK 时，schedule() 让出后
/// 重试。合作式调度下让出会给定时器中断/其他任务制造推进条件的机会：
/// timerfd 等到期、eventfd 等别人写、signalfd 等信号投递。
pub fn read(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    let (kind, nonblock) = unsafe { ((*obj(idx)).kind, (*obj(idx)).nonblock) };
    loop {
        let r = match kind {
            EvKind::Event => eventfd_read(fd, buf_ptr, len),
            EvKind::Timer => timerfd_read(fd, buf_ptr, len),
            EvKind::Signal => signalfd_read(fd, buf_ptr, len),
            EvKind::Inotify => inotify_read(fd, buf_ptr, len),
            EvKind::Epoll => -(EINVAL as i64), // epoll 不直接读
        };
        if r != -(EAGAIN as i64) || nonblock {
            return r;
        }
        // SAFETY: 系统调用上下文让出，同 rt_sigsuspend 的等待路径。
        unsafe { crate::sched::schedule() };
    }
}

/// 写一个匿名事件 fd。只有 eventfd 支持写。阻塞语义同 [`read`]：
/// 计数将溢出且非 O_NONBLOCK 时睡到有人读走。
pub fn write(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    let (kind, nonblock) = unsafe { ((*obj(idx)).kind, (*obj(idx)).nonblock) };
    if kind != EvKind::Event {
        return -(EINVAL as i64);
    }
    loop {
        let r = eventfd_write(fd, buf_ptr, len);
        if r != -(EAGAIN as i64) || nonblock {
            return r;
        }
        // SAFETY: 系统调用上下文让出。
        unsafe { crate::sched::schedule() };
    }
}
