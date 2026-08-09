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
- ~~COW 页面故障未处理导致 SIGSEGV~~（bug-cow）：`try_handle_cow_fault` 对
  引用计数为 0（未跟踪）的页返回 `Some(false)`，fork 后父进程写栈即被杀。
  已改为 refs<=1 直接授予写权限（保留 NX 位）。
- ~~取指故障（err bit4）在 present+user 页上未处理~~：traps.rs 现在对
  instruction-fetch 故障清除 NO_EXEC，避免代码页被误标 NX 后 SIGSEGV。

### 调试输出规范
- 写路径/系统调用路径的调试串口打印必须用 `pr_debug!`/`pr_warn!` 等
  分级宏，不要直接 `serial::print` 留 `0xNN(a0,a1)=ret` 之类的 trace。
- execve 里不要留 `EXEC: phdr/interp/LOAD/success` 之类的 trace 打印。

### 构建与运行（含 LFS busybox 测试）
- 默认 `scripts/build.sh` 不含 IDE 驱动（`extra-drivers` feature），
  LFS 模式会报「IDE not found」。
- 要跑 busybox shell：`bash scripts/build.sh --release --features extra-drivers`
  （debug+extra-drivers 会让 system 超过 0x9F000 安全区，链接 ASSERT 失败，
  必须用 release）。
- 用第二块 IDE 盘挂 lfs3.img 跑 shell：
  ```
  qemu-system-x86_64 \
    -drive format=raw,file=target/boot/shitix.img,if=ide \
    -drive format=raw,file=lfs3.img,if=ide \
    -m 256M -no-reboot -no-shutdown -display none -serial mon:stdio
  ```
- 环境需 nightly Rust（`rustup` 安装）+ `x86_64-unknown-none` target，
  以及 `as`/`ld`/`objcopy`（binutils）和 `qemu-system-x86_64`。

### 分支状态
- `fs`: 修复了 super_block.rs 的 guard 位置 bug
- `feat/networking-stack-rewrite`: 创建了网络栈骨架（已 push）
- `feat/drivers-misc-rewrite`: 重写剩余驱动和杂项代码（新分支）

### 驱动和杂项重写计划

#### 待重写的块设备驱动 (linux/drivers/block/)
| 文件 | 说明 | 优先级 |
|------|------|--------|
| `hd.c` | IDE 硬盘控制器 | 高 |
| `floppy.c` | 软盘控制器 | 低 |
| `genhd.c` | 通用磁盘接口 | 高 |

#### 待重写的字符设备 (linux/drivers/char/)
| 文件 | 说明 | 优先级 |
|------|------|--------|
| `serial.c` | 串口 (8250 UART) | 高 |
| `lp.c` | 并口打印机 | 低 |
| `psaux.c` | PS/2 鼠标 | 中 |
| `pty.c` | 伪终端 | 中 |

#### 待重写的网络设备驱动 (linux/drivers/net/)
| 文件 | 说明 | 优先级 |
|------|------|--------|
| `ne.c` | NE2000 网卡 | 高 |
| `3c509.c` | 3COM 以太网卡 | 中 |
| `slip.c` | SLIP 协议 | 中 |
| `plip.c` | 并口 IP | 低 |
| `skeleton.c` | 驱动骨架参考 | 参考 |

#### 内核杂项 (linux/kernel/)
| 文件 | 说明 | 优先级 |
|------|------|--------|
| `signal.c` | 信号处理 | 高 |
| `exit.c` | 进程退出 | 高 |
| `info.c` | 系统信息 | 低 |
| `ioport.c` | I/O 端口访问 | 中 |
| `module.c` | 模块加载 | 低 |

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
