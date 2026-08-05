//! 路由表。参考 `linux/net/inet/route.c`。
//!
//! ## 功能
//!
//! - 路由表管理
//! - 路由查找
//! - 默认网关
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `route.c` | 路由实现 |
//! | `route.h` | 路由结构 |


/// 路由标志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum RouteFlags {
    Up = 1 << 0,           // 路由开启
    Gateway = 1 << 1,      // 是网关
    Host = 1 << 2,         // 主机路由
    Net = 1 << 3,          // 网络路由
    Default = 1 << 4,      // 默认路由
}

/// 路由条目。参考 C 的 `struct rt_entry`。
#[derive(Debug, Clone, Copy)]
pub struct RouteEntry {
    /// 目标网络/主机地址。
    dest: u32,
    /// 目标掩码。
    mask: u32,
    /// 网关地址（0 表示直连）。
    gateway: u32,
    /// 网络设备下标（usize::MAX 表示无）。
    device: usize,
    /// 标志。
    flags: RouteFlags,
    /// 引用计数。
    refcnt: u16,
    /// 使用计数。
    use_: u32,
}

/// 路由表大小。
const ROUTE_TABLE_SIZE: usize = 32;

/// 路由表。
pub struct RouteTable {
    entries: [Option<RouteEntry>; ROUTE_TABLE_SIZE],
    /// 默认网关。
    default_gateway: u32,
    /// 默认设备下标（usize::MAX 表示无）。
    default_device: usize,
}

impl RouteTable {
    /// 创建新的路由表。
    pub const fn new() -> Self {
        RouteTable {
            entries: [None; ROUTE_TABLE_SIZE],
            default_gateway: 0,
            default_device: usize::MAX,
        }
    }

    /// 添加路由。
    ///
    /// # Safety
    ///
    /// - 表必须被锁定
    pub unsafe fn add_route(
        &mut self,
        dest: u32,
        mask: u32,
        gateway: u32,
        _dev: *mut crate::net::inet::dev::Device,
        flags: RouteFlags,
    ) {
        // SAFETY: 调用者保证锁定。
        // TODO: 实现真正的设备分配
        unsafe {
            for entry in &mut self.entries {
                if entry.is_none() {
                    *entry = Some(RouteEntry {
                        dest,
                        mask,
                        gateway,
                        device: usize::MAX, // TODO
                        flags,
                        refcnt: 0,
                        use_: 0,
                    });
                    return;
                }
            }
        }
    }

    /// 设置默认路由。
    pub fn set_default(&mut self, gateway: u32, _dev: *mut crate::net::inet::dev::Device) {
        self.default_gateway = gateway;
        self.default_device = usize::MAX; // TODO
    }

    /// 查找路由。
    ///
    /// # Safety
    ///
    /// - 表必须被锁定
    /// - 返回的路由条目可能被修改
    pub fn lookup(&self, daddr: u32) -> Option<&RouteEntry> {
        // 精确匹配查找
        for entry in &self.entries {
            if let Some(e) = entry {
                if (daddr & e.mask) == (e.dest & e.mask) {
                    return Some(e);
                }
            }
        }
        None
    }
}

/// 初始化路由表。参考 C 的 `ip_rt_init()`。
pub fn init() {
    // 设置默认路由（回环 + 默认网关）
}
