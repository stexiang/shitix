//! Unix 域套接字。参考 `linux/net/unix/`。
//!
//! ## 功能
//!
//! - 本地进程间通信
//! - 文件系统路径作为地址
//! - 流式（`SOCK_STREAM`）和 datagram（`SOCK_DGRAM`）支持
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `unix/sock.c` | Unix socket 实现 |
//! | `unix/proc.c` | /proc 接口 |
//!
//! ## SAFETY
//!
//! Unix socket 操作需要 `unsafe` 用于：
//!
//! - 路径字符串操作
//! - 引用计数（需要原子操作）
//!
//! 注意：当前项目可能暂时不实现完整的 Unix socket，
//! 因为 Linux 1.0.9 的实现比较复杂，涉及完整的 VFS 集成。

/// Unix socket 地址（文件系统路径）。
#[derive(Debug, Clone)]
pub struct UnixAddress {
    /// 路径（以 '\0' 结尾）。
    path: [u8; 108], // UNIX_PATH_MAX = 108
    /// 路径长度。
    len: usize,
}

impl UnixAddress {
    /// 从路径创建地址。
    pub fn new(path: &[u8]) -> Option<Self> {
        if path.len() >= 108 {
            return None;
        }
        let mut addr = UnixAddress {
            path: [0; 108],
            len: path.len(),
        };
        addr.path[..path.len()].copy_from_slice(path);
        Some(addr)
    }

    /// 获取路径。
    pub fn as_bytes(&self) -> &[u8] {
        &self.path[..self.len]
    }
}

/// Unix socket 连接状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixState {
    /// 空闲。
    Free,
    /// 正在连接。
    Connecting,
    /// 已连接。
    Connected,
    /// 已绑定。
    Bound,
}

/// Unix socket。
#[derive(Debug)]
pub struct UnixSocket {
    /// 地址。
    addr: Option<UnixAddress>,
    /// 状态。
    state: UnixState,
    /// 配对 socket（用于连接）。
    peer: Option<*mut UnixSocket>,
    /// 引用计数。
    refcnt: usize,
}

impl UnixSocket {
    /// 创建新的 Unix socket。
    ///
    /// # Safety
    ///
    /// - 返回的 socket 必须在不再需要时释放
    pub unsafe fn new() -> *mut Self {
        // TODO: 实现真正的 slab 分配器
        // SAFETY: 调用者负责内存管理。在单核内核中，无并发创建竞争。
        core::ptr::null_mut() // 占位
    }

    /// 绑定到路径。
    ///
    /// # Safety
    ///
    /// - `path` 必须是有效的文件系统路径
    pub fn bind(&mut self, path: &[u8]) -> i32 {
        self.addr = UnixAddress::new(path);
        self.state = UnixState::Bound;
        0
    }

    /// 释放 socket。
    ///
    /// # Safety
    ///
    /// - `sock` 必须是通过 `UnixSocket::new()` 创建的
    pub fn release(_sock: *mut Self) {
        // TODO: 实现真正的内存释放
    }
}

/// 初始化 Unix socket 子系统。
pub fn init() {
    // 初始化 Unix socket 表
}
