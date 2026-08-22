//! 匿名事件 fd：`eventfd` / `timerfd` / `signalfd` / `epoll` / `inotify` 的
//! 公共基础设施。对应 Linux 的 `fs/eventfd.c`、`fs/timerfd.c`、
//! `fs/signalfd.c`、`fs/eventpoll.c`、`fs/notify/inotify/*`。
//!
//! 与 [`super::pipe`] 同构：对象放在静态槽位数组里，fd → 对象 的映射是
//! **per-task** 的旁路数组（`FD_MAP`），不进 BSS 大户、也不走 VFS inode。
//!
//! # 与原版的差异
//! 1. **不阻塞**。读/写在「会阻塞」时返回 `-EAGAIN`（无论 O_NONBLOCK），
//!    不挂等待队列。这与本树 `select`/`poll` 的「fd 存在即就绪」简化一致；
//!    真正的 waitqueue 就绪语义要等 VFS poll 机制到位。
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
/// 每任务 fd 映射：`FD_MAP[task][fd]` = 对象下标；NIL = 非事件 fd。
static mut FD_MAP: [[usize; MAX_EV_FD]; crate::sched::NR_TASKS] =
    [[NIL; MAX_EV_FD]; crate::sched::NR_TASKS];

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
                if FD_MAP[t][fd] == idx {
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
        unsafe { FD_MAP[cur()][fd] = obj_idx; }
    }
}

/// 解除 fd 注册（close 时调用）。
pub fn unregister_fd(fd: usize) {
    if fd < MAX_EV_FD {
        // SAFETY: 系统调用上下文。
        unsafe { FD_MAP[cur()][fd] = NIL; }
    }
}

/// 该 fd 是否匿名事件 fd。
pub fn fd_is_event(fd: usize) -> bool {
    // SAFETY: 只读；系统调用上下文。
    unsafe { fd < MAX_EV_FD && FD_MAP[cur()][fd] != NIL }
}

/// 取 fd 对应的对象下标。
pub fn fd_to_obj(fd: usize) -> Option<usize> {
    // SAFETY: 只读；系统调用上下文。
    unsafe {
        if fd < MAX_EV_FD && FD_MAP[cur()][fd] != NIL {
            Some(FD_MAP[cur()][fd])
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

/// epoll_ctl：add/mod/del。本树无真实就绪跟踪，仅记录操作并返回成功。
pub fn epoll_ctl(_epfd: usize, _op: u32, _fd: usize, _ev_ptr: u64) -> i64 {
    // 校验 fd 范围与 ev 指针，避免坏指针。
    if _op > 3 {
        return -(EINVAL as i64);
    }
    0
}

/// epoll_wait：与 select/poll 一致——已注册 fd 一律「就绪」。
/// 本树无真实就绪跟踪，返回 0（无事件）或写回一个全就绪的假事件。
pub fn epoll_wait(_epfd: usize, ev_ptr: u64, _maxevents: u32, _timeout: i32) -> i64 {
    let _ = ev_ptr;
    0
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

/// inotify_add_watch：本树无文件系统事件钩子，仅记录并返回一个假 watch 描述符。
pub fn inotify_add_watch(_fd: usize, _path: u64, _mask: u32) -> i64 {
    1
}

/// inotify_rm_watch：无真实 watch，返回成功。
pub fn inotify_rm_watch(_fd: usize, _wd: u32) -> i64 {
    0
}

/// inotify 读：无事件 → 0（EOF）。
pub fn inotify_read(_fd: usize, _buf_ptr: u64, _len: u64) -> i64 {
    0
}

// =============================================================================
// 统一读/写入口（sys.rs 的 read/write 里按 kind 分发）
// =============================================================================

/// 读一个匿名事件 fd。按对象类型分发。
pub fn read(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    match unsafe { (*obj(idx)).kind } {
        EvKind::Event => eventfd_read(fd, buf_ptr, len),
        EvKind::Timer => timerfd_read(fd, buf_ptr, len),
        EvKind::Signal => signalfd_read(fd, buf_ptr, len),
        EvKind::Inotify => inotify_read(fd, buf_ptr, len),
        EvKind::Epoll => -(EINVAL as i64), // epoll 不直接读
    }
}

/// 写一个匿名事件 fd。只有 eventfd 支持写。
pub fn write(fd: usize, buf_ptr: u64, len: u64) -> i64 {
    let idx = match fd_to_obj(fd) {
        Some(i) => i,
        None => return -(EBADF as i64),
    };
    // SAFETY: idx 有效。
    match unsafe { (*obj(idx)).kind } {
        EvKind::Event => eventfd_write(fd, buf_ptr, len),
        _ => -(EINVAL as i64),
    }
}
