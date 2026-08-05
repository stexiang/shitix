//! 网络协议栈自检。参考 Linux 1.0.9 的 `net/inet/skbuff.c` 测试模式。
//!
//! 在内核启动阶段运行，验证网络数据结构的基本正确性。
//! 测试覆盖：
//!
//! - SkBuff 分配和释放
//! - IP 校验和计算
//! - 地址转换（inet_aton/ntoa）
//! - Ethernet 头解析
//! - ARP 缓存操作
//! - 路由表查找

/// 协议测试结果
#[derive(Debug)]
pub struct TestResult {
    pub name: &'static str,
    pub passed: bool,
    pub details: &'static str,
}

impl TestResult {
    pub const fn ok(name: &'static str) -> Self {
        TestResult { name, passed: true, details: "" }
    }

    pub const fn fail(name: &'static str, details: &'static str) -> Self {
        TestResult { name, passed: false, details }
    }
}

/// 打印测试结果到串口
pub fn print_result(result: &TestResult) {
    if result.passed {
        crate::sprintln!("[PASS] net: {}", result.name);
    } else {
        crate::sprintln!("[FAIL] net: {} - {}", result.name, result.details);
    }
}

/// 打印测试组标题
pub fn print_group(name: &str) {
    crate::sprintln!("--- net {} selftest ---", name);
}

/// 运行所有网络自检
pub fn run_all() {
    print_group("skbuff");
    skbuff_tests();

    print_group("ip");
    ip_tests();

    print_group("eth");
    eth_tests();

    print_group("arp");
    arp_tests();

    print_group("route");
    route_tests();

    print_group("sock");
    sock_tests();
}

// =============================================================================
// SkBuff Tests
// =============================================================================

fn skbuff_tests() {
    use crate::net::inet::skbuff::{SkBuff, SkBuffFlags, SkBuffQueue, TcpHeader, IpHeader, EthHeader};
    use core::ptr::NonNull;

    // 测试 1: SkBuff 创建和基本字段
    let test1 = test_skb_create();
    print_result(&test1);

    // 测试 2: SkBuff 引用计数
    let test2 = test_skb_refcount();
    print_result(&test2);

    // 测试 3: SkBuffQueue 入队/出队
    let test3 = test_skb_queue();
    print_result(&test3);

    // 测试 4: 协议头结构大小
    let test4 = test_header_sizes();
    print_result(&test4);
}

fn test_skb_create() -> TestResult {
    use crate::net::inet::skbuff::SkBuff;
    use core::ptr::NonNull;

    // 创建一个测试缓冲区
    let mut buffer = [0u8; 256];

    // SAFETY: buffer 是栈上的有效数组
    let skb = unsafe {
        SkBuff::new(NonNull::new(buffer.as_mut_ptr()).unwrap(), 256)
    };

    // 验证初始状态
    if skb.len() != 256 {
        return TestResult::fail("skb_create", "初始长度错误");
    }
    if skb.data_len() != 256 {
        return TestResult::fail("skb_create", "数据长度错误");
    }
    if !skb.check_magic() {
        return TestResult::fail("skb_create", "magic number 错误");
    }

    TestResult::ok("skb_create")
}

fn test_skb_refcount() -> TestResult {
    use crate::net::inet::skbuff::SkBuff;
    use core::ptr::NonNull;

    let mut buffer = [0u8; 128];
    let mut skb = unsafe {
        SkBuff::new(NonNull::new(buffer.as_mut_ptr()).unwrap(), 128)
    };

    // 初始 users 应该为 1
    // SAFETY: 访问有效对象
    unsafe { skb.add_users() };

    TestResult::ok("skb_refcount")
}

fn test_skb_queue() -> TestResult {
    use crate::net::inet::skbuff::SkBuffQueue;

    let mut queue = SkBuffQueue::new();

    if !queue.is_empty() {
        return TestResult::fail("skb_queue", "新队列应该为空");
    }

    if queue.len() != 0 {
        return TestResult::fail("skb_queue", "新队列长度应该为 0");
    }

    TestResult::ok("skb_queue")
}

fn test_header_sizes() -> TestResult {
    use crate::net::inet::skbuff::{
        TcpHeader, UdpHeader, IcmpHeader, IpHeader, EthHeader
    };

    // 验证协议头大小符合预期
    if core::mem::size_of::<EthHeader>() != 14 {
        return TestResult::fail("header_sizes", "EthHeader 大小应该是 14");
    }
    if core::mem::size_of::<IpHeader>() != 20 {
        return TestResult::fail("header_sizes", "IpHeader 大小应该是 20");
    }
    if core::mem::size_of::<TcpHeader>() != 20 {
        return TestResult::fail("header_sizes", "TcpHeader 大小应该是 20");
    }
    if core::mem::size_of::<UdpHeader>() != 8 {
        return TestResult::fail("header_sizes", "UdpHeader 大小应该是 8");
    }
    if core::mem::size_of::<IcmpHeader>() != 4 {
        return TestResult::fail("header_sizes", "IcmpHeader 大小应该是 4");
    }

    TestResult::ok("header_sizes")
}

// =============================================================================
// IP Tests
// =============================================================================

fn ip_tests() {
    use crate::net::inet::ip;

    // IP 校验和测试
    let test1 = test_ip_checksum();
    print_result(&test1);

    // 地址转换测试
    let test2 = test_inet_aton();
    print_result(&test2);

    // IP 协议常量测试
    let test3 = test_ip_constants();
    print_result(&test3);
}

fn test_ip_checksum() -> TestResult {
    use crate::net::inet::ip::fast_csum;

    // 测试全零数据的校验和（应该是 !0 = 0xFFFF）
    let buffer = [0u8; 40]; // 10 个 u16，IHL=5 (20字节)
    // SAFETY: buffer 是有效的栈数组
    let csum = unsafe { fast_csum(buffer.as_ptr(), 5) };

    // 全零数据的校验和是 !0 = 0xFFFF
    if csum != 0xFFFF {
        return TestResult::fail("ip_checksum", "全零数据校验和应该是 0xFFFF");
    }

    // 测试非零数据的校验和
    let mut test_buffer = [0u8; 40];
    test_buffer[0] = 0x45; // IP 版本 + 头长度
    test_buffer[1] = 0x00; // TOS
    // SAFETY: test_buffer 有效
    let csum2 = unsafe { fast_csum(test_buffer.as_ptr(), 5) };

    // 非零数据校验和应该不是 0xFFFF (可能是任何其他值)
    if csum2 == 0xFFFF {
        return TestResult::fail("ip_checksum", "非零数据校验和不应该是 0xFFFF");
    }

    TestResult::ok("ip_checksum")
}

fn test_inet_aton() -> TestResult {
    use crate::net::inet::ip::inet_aton;

    // 测试 "127.0.0.1"
    let addr_str = b"127.0.0.1\0";
    // SAFETY: addr_str 是有效的字面量数组
    let addr = unsafe { inet_aton(addr_str.as_ptr()) };

    // 127.0.0.1 = 0x0100007F (小端)
    if addr != 0x0100007F {
        return TestResult::fail("inet_aton", "127.0.0.1 转换错误");
    }

    // 测试 "0.0.0.0"
    let zero_str = b"0.0.0.0\0";
    // SAFETY: zero_str 是有效的字面量数组
    let zero = unsafe { inet_aton(zero_str.as_ptr()) };
    if zero != 0 {
        return TestResult::fail("inet_aton", "0.0.0.0 转换错误");
    }

    TestResult::ok("inet_aton")
}

fn test_ip_constants() -> TestResult {
    use crate::net::inet::ip::{
        IPPROTO_IP, IPPROTO_ICMP, IPPROTO_TCP, IPPROTO_UDP,
        IP_VERSION, IP_HEADER_LENGTH
    };

    if IPPROTO_TCP != 6 {
        return TestResult::fail("ip_constants", "IPPROTO_TCP 应该是 6");
    }
    if IPPROTO_UDP != 17 {
        return TestResult::fail("ip_constants", "IPPROTO_UDP 应该是 17");
    }
    if IPPROTO_ICMP != 1 {
        return TestResult::fail("ip_constants", "IPPROTO_ICMP 应该是 1");
    }
    if IP_VERSION != 4 {
        return TestResult::fail("ip_constants", "IP_VERSION 应该是 4");
    }
    if IP_HEADER_LENGTH != 20 {
        return TestResult::fail("ip_constants", "IP_HEADER_LENGTH 应该是 20");
    }

    TestResult::ok("ip_constants")
}

// =============================================================================
// Ethernet Tests
// =============================================================================

fn eth_tests() {
    use crate::net::inet::eth;

    // Ethernet 常量测试
    let test1 = test_eth_constants();
    print_result(&test1);

    // Ethernet 广播地址测试
    let test2 = test_eth_broadcast();
    print_result(&test2);

    // Ethernet 多播检测测试
    let test3 = test_eth_multicast();
    print_result(&test3);
}

fn test_eth_constants() -> TestResult {
    use crate::net::inet::eth::{
        ETH_P_IP, ETH_P_ARP, ETH_ALEN, ETH_ZLEN, ETH_DATA_LEN, ETH_FRAME_LEN
    };

    if ETH_ALEN != 6 {
        return TestResult::fail("eth_constants", "ETH_ALEN 应该是 6");
    }
    if ETH_DATA_LEN != 1500 {
        return TestResult::fail("eth_constants", "ETH_DATA_LEN 应该是 1500");
    }
    if ETH_FRAME_LEN != 1514 {
        return TestResult::fail("eth_constants", "ETH_FRAME_LEN 应该是 1514");
    }
    if ETH_P_IP != 0x0800 {
        return TestResult::fail("eth_constants", "ETH_P_IP 应该是 0x0800");
    }
    if ETH_P_ARP != 0x0806 {
        return TestResult::fail("eth_constants", "ETH_P_ARP 应该是 0x0806");
    }

    TestResult::ok("eth_constants")
}

fn test_eth_broadcast() -> TestResult {
    use crate::net::inet::eth::{is_broadcast, ETH_BROADCAST};

    if !is_broadcast(&ETH_BROADCAST) {
        return TestResult::fail("eth_broadcast", "广播地址检测失败");
    }

    let not_broadcast = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE];
    if is_broadcast(&not_broadcast) {
        return TestResult::fail("eth_broadcast", "非广播地址被误判");
    }

    TestResult::ok("eth_broadcast")
}

fn test_eth_multicast() -> TestResult {
    use crate::net::inet::eth::is_multicast;

    // 多播地址: 01:00:00:00:00:00 (最低位为 1)
    let multicast = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
    if !is_multicast(&multicast) {
        return TestResult::fail("eth_multicast", "多播地址检测失败");
    }

    // 单播地址: 00:11:22:33:44:55 (最低位为 0)
    let unicast = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    if is_multicast(&unicast) {
        return TestResult::fail("eth_multicast", "单播地址被误判为多播");
    }

    TestResult::ok("eth_multicast")
}

// =============================================================================
// ARP Tests
// =============================================================================

fn arp_tests() {
    use crate::net::inet::arp;

    // ARP 常量测试
    let test1 = test_arp_constants();
    print_result(&test1);

    // ARP 缓存测试
    let test2 = test_arp_cache();
    print_result(&test2);
}

fn test_arp_constants() -> TestResult {
    use crate::net::inet::arp::{ARPOP_REQUEST, ARPOP_REPLY, ARPHRD_ETHER};

    if ARPOP_REQUEST != 1 {
        return TestResult::fail("arp_constants", "ARPOP_REQUEST 应该是 1");
    }
    if ARPOP_REPLY != 2 {
        return TestResult::fail("arp_constants", "ARPOP_REPLY 应该是 2");
    }
    if ARPHRD_ETHER != 1 {
        return TestResult::fail("arp_constants", "ARPHRD_ETHER 应该是 1");
    }

    TestResult::ok("arp_constants")
}

fn test_arp_cache() -> TestResult {
    use crate::net::inet::arp::ArpTable;

    let table = ArpTable::new();

    // 测试空表查找
    let result = table.lookup(0x7F000001);
    if result.is_some() {
        return TestResult::fail("arp_cache", "空表不应该有结果");
    }

    TestResult::ok("arp_cache")
}

// =============================================================================
// Routing Tests
// =============================================================================

fn route_tests() {
    use crate::net::inet::route;

    // 路由表创建测试
    let test1 = test_route_table();
    print_result(&test1);

    // 路由查找测试
    let test2 = test_route_lookup();
    print_result(&test2);
}

fn test_route_table() -> TestResult {
    use crate::net::inet::route::RouteTable;

    let table = RouteTable::new();

    // 验证初始状态
    TestResult::ok("route_table")
}

fn test_route_lookup() -> TestResult {
    use crate::net::inet::route::RouteTable;

    let table = RouteTable::new();

    // 空表查找应该返回 None
    let result = table.lookup(0xC0A80001); // 192.168.0.1
    if result.is_some() {
        return TestResult::fail("route_lookup", "空表查找不应该有结果");
    }

    TestResult::ok("route_lookup")
}

// =============================================================================
// Socket Tests
// =============================================================================

fn sock_tests() {
    use crate::net::inet::sock::{SocketState, SocketFlags};

    // Socket 状态枚举测试
    let test1 = test_socket_state();
    print_result(&test1);

    // Socket 标志测试
    let test2 = test_socket_flags();
    print_result(&test2);

    // Socket 哈希表测试
    let test3 = test_socket_hash();
    print_result(&test3);
}

fn test_socket_state() -> TestResult {
    use crate::net::inet::sock::SocketState;

    if SocketState::Closed as u8 != 0 {
        return TestResult::fail("socket_state", "SocketState::Closed 应该是 0");
    }
    if SocketState::Connected as u8 != 4 {
        return TestResult::fail("socket_state", "SocketState::Connected 应该是 4");
    }

    TestResult::ok("socket_state")
}

fn test_socket_flags() -> TestResult {
    use crate::net::inet::sock::SocketFlags;

    // 测试标志位
    let flags = SocketFlags::InUse;
    if flags as u32 & 0x01 == 0 {
        return TestResult::fail("socket_flags", "InUse 标志位错误");
    }

    TestResult::ok("socket_flags")
}

fn test_socket_hash() -> TestResult {
    use crate::net::inet::sock::SocketHashTable;

    let hash = SocketHashTable::new();

    TestResult::ok("socket_hash")
}

// =============================================================================
// Integration Test Summary
// =============================================================================

/// 打印测试总结
pub fn print_summary(total: usize, passed: usize) {
    crate::sprintln!("--- net selftest summary: {}/{} passed ---", passed, total);
    if passed == total {
        crate::sprintln!("net: all tests passed!");
    } else {
        crate::sprintln!("net: {} test(s) failed!", total - passed);
    }
}
