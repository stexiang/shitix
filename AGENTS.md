## 项目特定知识

### 构建命令
- `bash scripts/build.sh` - 构建内核镜像
- `bash scripts/test.sh` - 构建并运行测试
- `bash scripts/test.sh run` - 交互式运行
- `bash scripts/test.sh debug` - GDB 调试

### 内存布局
| 地址 | 内容 |
|------|------|
| 0x90000 | 机器参数区 |
| 0x90200 | setup（4扇区）|
| 0x9E000 | E820 条目数组 |
| 0x4000-0x6FFF | 页表（PML4/PDPT/PD）|
| 0x10000 | system（head.S + 内核）|

### 已解决的历史问题
- ~~LLVM noalias 优化导致 super block 数据竞争~~（bug-023）
- ~~内存溢出（OOM）导致 panic~~

### 分支状态
- `fs`: 修复了 super_block.rs 的 guard 位置 bug
- `feat/networking-stack-rewrite`: 创建了网络栈骨架（已 push）

### 网络栈实现状态
已添加骨架代码（2026-08-05）：
- `src/net/mod.rs` - 模块根
- `src/net/inet/skbuff.rs` - Socket buffer
- `src/net/inet/sock.rs` - Socket/协议控制块
- `src/net/inet/ip.rs` - IP 协议
- `src/net/inet/tcp.rs` - TCP 协议存根
- `src/net/inet/udp.rs` - UDP 协议存根
- `src/net/inet/icmp.rs` - ICMP 协议存根
- `src/net/inet/arp.rs` - ARP 缓存
- `src/net/inet/route.rs` - 路由表
- `src/net/inet/protocol.rs` - 协议注册
- `src/net/inet/eth.rs` - Ethernet 处理
- `src/net/inet/dev.rs` - 网络设备接口
- `src/net/unix/mod.rs` - Unix domain socket
- `src/net/tests.rs` - 自检测试（17 个测试全部通过）

自检测试覆盖：
- SkBuff: 创建、引用计数、队列、头部大小
- IP: 校验和、地址转换、协议常量
- Ethernet: 协议常量、广播/多播检测
- ARP: 协议常量、缓存操作
- 路由: 表创建、查找
- Socket: 状态枚举、标志、哈希表

TODO:
- [ ] 实现 slab 分配器
- [ ] 完成协议处理器
- [ ] 添加设备驱动
- [ ] 集成 syscall 层
