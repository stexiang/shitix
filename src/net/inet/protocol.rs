//! 协议注册和分发。参考 `linux/net/inet/protocol.c`。
//!
//! ## 功能
//!
//! - 注册网络层协议处理器
//! - 协议号到处理器的映射
//! - IP 层的协议查找
//!
//! ## C 源码对照
//!
//! | C 文件 | 说明 |
//! |--------|------|
//! | `protocol.c` | 协议注册 |
//! | `protocol.h` | 协议结构 |

use crate::net::inet::skbuff::SkBuff;

/// 协议处理函数类型。
pub type ProtocolHandler = unsafe fn(skb: *mut SkBuff) -> i32;

/// 协议描述符槽（内核无全局堆，用定长槽位表代替 C 的 inet_protocol 链表）。
#[derive(Clone, Copy, Default)]
struct ProtoSlot {
    used: bool,
    protocol: u8,
    name: &'static str,
    handler: Option<ProtocolHandler>,
}

const MAX_PROTOCOLS: usize = 8;
static mut PROTO_SLOTS: [ProtoSlot; MAX_PROTOCOLS] = [ProtoSlot {
    used: false,
    protocol: 0,
    name: "",
    handler: None,
}; MAX_PROTOCOLS];

fn slot(i: usize) -> ProtoSlot {
    // SAFETY: 槽位表仅在协议层 init/注册路径写，运行期只读。
    unsafe { core::ptr::read(&raw const PROTO_SLOTS[i]) }
}

/// 注册协议处理器。
///
/// # Safety
/// 必须在系统初始化上下文调用（UP 启动期，无竞争）。
pub fn inet_add_protocol(protocol: u8, handler: ProtocolHandler, name: &'static str) -> i32 {
    unsafe {
        for i in 0..MAX_PROTOCOLS {
            let cur = slot(i);
            if cur.used && cur.protocol == protocol {
                return -1; // EEXIST
            }
            if !cur.used {
                PROTO_SLOTS[i] = ProtoSlot {
                    used: true,
                    protocol,
                    name,
                    handler: Some(handler),
                };
                return 0;
            }
        }
    }
    -1 // ENOSPC
}

/// 移除协议处理器。
pub fn inet_del_protocol(protocol: u8) -> i32 {
    unsafe {
        for i in 0..MAX_PROTOCOLS {
            let cur = slot(i);
            if cur.used && cur.protocol == protocol {
                *get_slot_mut(i) = ProtoSlot::default();
                return 0;
            }
        }
    }
    -1
}

fn get_slot_mut(i: usize) -> &'static mut ProtoSlot {
    // SAFETY: 同 slot()。
    unsafe { &mut (*get_mut_ptr() )[i] }
}
fn get_mut_ptr() -> *mut [ProtoSlot; MAX_PROTOCOLS] {
    unsafe { &raw mut PROTO_SLOTS }
}

/// 根据协议号查找处理器并投递。未注册 → -1。
///
/// # Safety
/// 调用者保证 `skb` 有效；handler 的消耗语义由注册方保证。
pub fn inet_protocol(protocol: u8, skb: *mut SkBuff) -> i32 {
    for i in 0..MAX_PROTOCOLS {
        let cur = slot(i);
        if cur.used && cur.protocol == protocol {
            if let Some(h) = cur.handler {
                // SAFETY: 注册方的契约。
                return unsafe { h(skb) };
            }
            return -1;
        }
    }
    -1
}

/// 初始化协议层。参考 C 的 `inet_proto_init()`。当前数据路径在
/// netif.rs 里直接分派（proto match），注册表供后续 SkBuff 化收包使用；
/// 这里把表清空，避免静态残留。
pub fn init() {
    // SAFETY: 启动期单线程。
    unsafe {
        for i in 0..MAX_PROTOCOLS {
            *get_slot_mut(i) = ProtoSlot {
                used: false,
                protocol: 0,
                name: "",
                handler: None,
            };
        }
    }
}
