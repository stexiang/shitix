//! Socket 管理。参考 `linux/net/inet/sock.c` 和 `linux/net/inet/sock.h`。
//!
//! `Socket`（内核中的 `struct sock`）是 INET 协议族 socket 的核心数据结构。
//! 注意：这里的 `Socket` 对应 C 的 `struct sock`（协议控制块），
//! 不是 BSD socket API 的 `struct socket`。
//!
//! ## 与 C 的差异
//!
//! | C 结构 | Rust 等价 | 说明 |
//! |--------|-----------|------|
//! | `struct sock` | `Socket` | 协议控制块（内核内部）|
//! | `struct socket` | 不移植 | 用户可见的 socket 对象（已在 fs/pipe.rs 简化）|
//!
//! ## Socket 状态（C 的 `volatile char state`）
//!
//! - `Closed`：关闭
//! - `Open`：打开
//! - `Listening`：监听中
//! - `Connecting`：连接中
//! - `Connected`：已连接
//! - `Disconnecting`：断开中
//!
//! ## SAFETY
//!
//! `Socket` 操作需要 `unsafe` 的原因：
//!
//! 1. **并发访问**：多个任务可能同时操作 socket
//! 2. **中断上下文**：设备中断可能触发 socket 回调
//! 3. **DMA**：socket 持有 sk_buff，这些缓冲区可能被设备 DMA 写入
//!
//! 每处 `unsafe` 必须有 SAFETY 注释解释为什么在当前上下文中安全。

use crate::net::inet::skbuff::{SkBuff, SkBuffQueue};
use crate::sched::WaitQueue;
use core::ptr::NonNull;

/// Socket 状态。参考 C 的 `volatile char state`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SocketState {
    Closed = 0,
    Open = 1,
    Listening = 2,
    Connecting = 3,
    Connected = 4,
    Disconnecting = 5,
}

/// Socket/协议控制块。参考 C 的 `struct sock`。
///
/// # Memory Layout
///
/// ```text
/// struct Socket {
///     // 内存管理
///     wmem_alloc: AtomicU32,    // 已分配写内存
///     rmem_alloc: AtomicU32,    // 已分配读内存
///     
///     // 序列号（TCP）
///     write_seq: u32,           // 下一个要发送的字节
///     sent_seq: u32,            // 已发送但未确认
///     rcv_ack_seq: u32,         // 已接收确认的最高序列号
///     
///     // Socket 状态
///     state: SocketState,       // 当前状态
///     flags: SocketFlags,       // 各种标志
///     
///     // 队列
///     write_queue: SkBuffQueue, // 待发送队列
///     receive_queue: SkBuffQueue, // 接收队列
///     back_log: Option<NonNull<SkBuff>>, // backlog 队列
///     
///     // 地址
///     saddr: u32,              // 源 IP
///     daddr: u32,              // 目的 IP
///     sport: u16,              // 源端口
///     dport: u16,              // 目的端口
///     
///     // 窗口（TCP）
///     window: u16,              // 接收窗口大小
///     mss: u16,                // 最大段大小
///     
///     // 等待队列
///     sleep: WaitQueue,
///     
///     // 协议特定
///     protocol: u8,             // IPPROTO_*  
///     type_: u8,                // SOCK_* type
/// }
/// ```
///
/// ## Safety
///
/// - `write_queue` 和 `receive_queue` 的操作需要锁保护
/// - 中断回调（`data_ready`, `write_space`）必须在持有锁时调用
/// - `back_log` 处理涉及中断上下文和进程上下文的交互
pub struct Socket {
    /// 已分配的写内存字节数（C 的 `volatile unsigned long wmem_alloc`）。
    /// 原子操作以支持未来多核。
    wmem_alloc: u32,

    /// 已分配的读内存字节数（C 的 `volatile unsigned long rmem_alloc`）。
    rmem_alloc: u32,

    /// 下一个要发送的字节序列号（C 的 `write_seq`）。
    write_seq: u32,

    /// 已发送但未确认的最高序列号（C 的 `sent_seq`）。
    sent_seq: u32,

    /// 已接收确认的最高序列号（C 的 `rcv_ack_seq`）。
    rcv_ack_seq: u32,

    /// Socket 状态。
    state: SocketState,

    /// Socket 标志。
    flags: SocketFlags,

    /// 待发送队列（C 的 `volatile send_tail/send_head`）。
    write_queue: SkBuffQueue,

    /// 接收队列（C 的 `volatile rqueue`）。
    receive_queue: SkBuffQueue,

    /// Backlog 队列（C 的 `volatile back_log`）。
    back_log: Option<NonNull<SkBuff>>,

    /// 源 IP 地址（C 的 `saddr`）。
    saddr: u32,

    /// 目的 IP 地址（C 的 `daddr`）。
    daddr: u32,

    /// 源端口（C 的 `num` 字段存储端口）。
    sport: u16,

    /// 目的端口（C 的 `dport`）。
    dport: u16,

    /// 接收窗口大小（C 的 `window`）。
    window: u16,

    /// 最大段大小（C 的 `mss`）。
    mss: u16,

    /// 等待队列（C 的 `**sleep`）。
    sleep: WaitQueue,

    /// 协议号（IPPROTO_*）。
    protocol: u8,

    /// Socket 类型（SOCK_*）。
    type_: u8,

    /// TTL（C 的 `ip_ttl`）。
    ttl: u8,

    /// TOS（C 的 `ip_tos`）。
    tos: u8,
}

/// Socket 标志。参考 C 的 `volatile char` 字段集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SocketFlags {
    None = 0,
    InUse = 1 << 0,           // 正在使用
    Dead = 1 << 1,            // socket 已死
    UrgentInline = 1 << 2,    // 紧急数据内联
    Interrupt = 1 << 3,       // 中断模式
    Blog = 1 << 4,            // 日志
    Done = 1 << 5,            // 操作完成
    Reuse = 1 << 6,           // 可重用（TIME_WAIT）
    KeepOpen = 1 << 7,        // 保活
    Linger = 1 << 8,          // 延迟关闭
    DelayAcks = 1 << 9,       // 延迟 ACK
    Destroy = 1 << 10,        // 需销毁
    NoCheck = 1 << 11,        // 禁用校验和
    Broadcast = 1 << 12,      // 广播
    Nonagle = 1 << 13,        // Nagle 算法禁用
}

const MAX_SOCK_POOL: usize = 8;
static mut SOCK_POOL: [core::mem::MaybeUninit<Socket>; MAX_SOCK_POOL] =
    [const { core::mem::MaybeUninit::uninit() }; MAX_SOCK_POOL];
static mut SOCK_POOL_USED: [bool; MAX_SOCK_POOL] = [false; MAX_SOCK_POOL];

impl Socket {
    /// 创建新 socket。参考 C 的 `struct sock *sk_alloc()`。
    ///
    /// 内核没有全局堆，用 8 槽静态池充当 slab（地址稳定、free 只翻位）。
    ///
    /// # Safety
    ///
    /// - 返回的 socket 必须在不再需要时通过 [`Socket::free`] 释放
    /// - 池满返回 null
    pub unsafe fn new(protocol: u8, type_: u8) -> *mut Self {
        for i in 0..MAX_SOCK_POOL {
            // SAFETY: 池只在协议创建/销毁路径动；socket 层不并发创建。
            unsafe {
                if !SOCK_POOL_USED[i] {
                    SOCK_POOL_USED[i] = true;
                    let sk = SOCK_POOL[i].as_mut_ptr();
                    sk.write(Socket::blank(protocol, type_));
                    return sk;
                }
            }
        }
        core::ptr::null_mut()
    }

    fn blank(protocol: u8, type_: u8) -> Self {
        Socket {
            wmem_alloc: 0,
            rmem_alloc: 0,
            write_seq: 0,
            sent_seq: 0,
            rcv_ack_seq: 0,
            state: SocketState::Closed,
            flags: SocketFlags::None,
            write_queue: SkBuffQueue::new(),
            receive_queue: SkBuffQueue::new(),
            back_log: None,
            saddr: 0,
            daddr: 0,
            sport: 0,
            dport: 0,
            window: 0,
            mss: 0,
            sleep: WaitQueue::new(),
            protocol,
            type_,
            ttl: 64,
            tos: 0,
        }
    }

    /// 释放 socket。参考 C 的 `void sk_free(struct sock *sk)`。
    ///
    /// # Safety
    ///
    /// - `sk` 必须是 [`Socket::new`] 返回的池内指针
    /// - 调用后 `sk` 不能再使用（池外指针/重复释放都是 no-op）
    pub fn free(sk: *mut Self) {
        if sk.is_null() {
            return;
        }
        for i in 0..MAX_SOCK_POOL {
            // SAFETY: 槽位地址固定，比对不触碰内容。
            unsafe {
                if SOCK_POOL[i].as_mut_ptr() == sk {
                    SOCK_POOL_USED[i] = false;
                    return;
                }
            }
        }
    }

    /// 源 IP（C 的 `saddr`）。
    pub fn saddr(&self) -> u32 {
        self.saddr
    }

    /// 设置源 IP。
    pub fn set_saddr(&mut self, addr: u32) {
        self.saddr = addr;
    }

    /// 检查 socket 是否正在使用。
    pub fn in_use(&self) -> bool {
        self.wmem_alloc != 0 || self.rmem_alloc != 0
    }

    /// 获取当前状态。
    pub fn state(&self) -> SocketState {
        self.state
    }

    /// 设置状态。
    ///
    /// # Safety
    ///
    /// - 必须持有 socket 锁
    /// - 状态转换必须符合协议状态机
    pub unsafe fn set_state(&mut self, state: SocketState) {
        self.state = state;
    }

    /// 添加数据到接收队列（C 的 `skb_queue_tail(&sk->receive_queue, skb)`）。
    ///
    /// # Safety
    ///
    /// - `skb` 必须有效
    /// - 必须在持有锁时调用
    /// - 可能唤醒等待中的任务
    pub unsafe fn receive_queue_add(&mut self, skb: *mut SkBuff) {
        // SAFETY: 调用者保证 `skb` 有效且 `self` 被锁定。
        unsafe {
            self.receive_queue.queue_tail(skb);
            self.rmem_alloc += (*skb).len() as u32;
        }
        // 唤醒等待读取的任务
        self.sleep.wake_up();
    }

    /// 从接收队列取数据（C 的 `skb_dequeue(&sk->receive_queue)`）。
    ///
    /// # Safety
    ///
    /// - 调用前必须持有锁
    /// - 返回的 skb 必须由调用者处理
    pub unsafe fn receive_queue_remove(&mut self) -> *mut SkBuff {
        // SAFETY: 调用者保证 `self` 被锁定。
        unsafe {
            let skb = self.receive_queue.dequeue();
            if !skb.is_null() {
                self.rmem_alloc -= (*skb).len() as u32;
            }
            skb
        }
    }

    /// 获取接收队列长度。
    pub fn receive_queue_len(&self) -> usize {
        self.receive_queue.len()
    }

    /// 添加数据到发送队列。
    ///
    /// # Safety
    ///
    /// - `skb` 必须有效
    /// - 必须在持有锁时调用
    pub unsafe fn write_queue_add(&mut self, skb: *mut SkBuff) {
        // SAFETY: 调用者保证 `skb` 有效且 `self` 被锁定。
        unsafe {
            self.write_queue.queue_tail(skb);
            self.wmem_alloc += (*skb).len() as u32;
        }
    }

    /// 等待读取数据。
    ///
    /// # Safety
    ///
    /// - 必须在进程上下文调用（会睡）
    pub unsafe fn wait_for_data(&mut self) {
        // SAFETY: 进程上下文，可睡。
        unsafe { self.sleep.sleep_on() };
    }

    /// 唤醒等待中的任务。
    pub fn wake_up(&mut self) {
        self.sleep.wake_up();
    }

    /// 获取目的地址。
    pub fn daddr(&self) -> u32 {
        self.daddr
    }

    /// 设置目的地址。
    pub fn set_daddr(&mut self, addr: u32) {
        self.daddr = addr;
    }

    /// 获取目的端口。
    pub fn dport(&self) -> u16 {
        self.dport
    }

    /// 设置目的端口。
    pub fn set_dport(&mut self, port: u16) {
        self.dport = port;
    }

    /// 获取源端口。
    pub fn sport(&self) -> u16 {
        self.sport
    }

    /// 设置源端口。
    pub fn set_sport(&mut self, port: u16) {
        self.sport = port;
    }

    /// 获取窗口大小。
    pub fn window(&self) -> u16 {
        self.window
    }

    /// 设置窗口大小。
    pub fn set_window(&mut self, window: u16) {
        self.window = window;
    }

    /// 获取 MSS。
    pub fn mss(&self) -> u16 {
        self.mss
    }

    /// 获取 TTL。
    pub fn ttl(&self) -> u8 {
        self.ttl
    }

    /// 获取 TOS。
    pub fn tos(&self) -> u8 {
        self.tos
    }
}

/// Socket 数组（端口哈希表）。参考 C 的 `sock_array[SOCK_ARRAY_SIZE]`。
///
/// # Safety
///
/// - 数组访问需要锁保护
/// - 桶链表按协议和端口组织
pub struct SocketHashTable {
    /// 哈希桶数组。每个桶是一个 socket 链表。
    /// 大小为 64（C 的 `SOCK_ARRAY_SIZE`）。
    buckets: [Option<NonNull<Socket>>; 64],
}

impl SocketHashTable {
    /// 创建空哈希表。
    pub const fn new() -> Self {
        SocketHashTable {
            buckets: [None; 64],
        }
    }

    /// 计算哈希值（端口 + 目的地址）。
    ///
    /// # Safety
    ///
    /// - 在持有锁的情况下调用
    fn hash(&self, protocol: u16, daddr: u32, dport: u16) -> usize {
        // 参考 C 的哈希函数
        let shifted = (dport as usize) << 3;
        ((protocol as usize ^ daddr as usize ^ ((dport as usize) >> 16))
            ^ shifted)
            & (self.buckets.len() - 1)
    }

    /// 查找 socket。
    ///
    /// # Safety
    ///
    /// - 必须在持有锁时调用
    pub unsafe fn lookup(
        &self,
        protocol: u16,
        daddr: u32,
        dport: u16,
        saddr: u32,
        sport: u16,
    ) -> Option<NonNull<Socket>> {
        let h = self.hash(protocol, daddr, dport);
        
        // SAFETY: 哈希值在范围内，链表访问在持有锁时进行。
        unsafe {
            let mut curr = self.buckets[h];
            while let Some(ptr) = curr {
                let sk = &*ptr.as_ptr();
                if sk.daddr == daddr && sk.dport == dport
                    && sk.saddr == saddr && sk.sport == sport
                    && sk.protocol == protocol as u8
                {
                    return Some(ptr);
                }
                curr = (*ptr.as_ptr()).next();
            }
        }
        None
    }

    /// 插入 socket 到哈希表。
    ///
    /// # Safety
    ///
    /// - `sk` 必须有效且不在任何哈希表中
    /// - 必须在持有锁时调用
    pub unsafe fn insert(&mut self, sk: *mut Socket) {
        let sock = &*sk;
        let h = self.hash(sock.protocol as u16, sock.daddr, sock.dport);
        
        // SAFETY: 哈希值已校验，`buckets[h]` 访问安全。
        unsafe {
            // 头插法
            (*sk).set_next(self.buckets[h]);
            self.buckets[h] = NonNull::new(sk);
        }
    }

    /// 从哈希表移除 socket。
    ///
    /// # Safety
    ///
    /// - `sk` 必须在哈希表中
    /// - 必须在持有锁时调用
    pub unsafe fn remove(&mut self, sk: *mut Socket) {
        let sock = &*sk;
        let h = self.hash(sock.protocol as u16, sock.daddr, sock.dport);
        
        let mut prev: Option<NonNull<Socket>> = None;
        let mut curr = self.buckets[h];
        
        // SAFETY: 链表遍历在持有锁时进行。
        unsafe {
            while let Some(ptr) = curr {
                if ptr.as_ptr() == sk {
                    match prev {
                        Some(p) => (*p.as_ptr()).set_next((*sk).next()),
                        None => self.buckets[h] = (*sk).next(),
                    }
                    (*sk).set_next(None);
                    return;
                }
                prev = curr;
                curr = (*ptr.as_ptr()).next();
            }
        }
    }
}

impl Socket {
    /// 获取下一个 socket（用于链表遍历）。
    fn next(&self) -> Option<NonNull<Socket>> {
        // 在实际实现中需要添加 next 字段
        None
    }

    /// 设置下一个 socket。
    fn set_next(&mut self, _next: Option<NonNull<Socket>>) {
        // 在实际实现中需要添加 next 字段
    }
}
