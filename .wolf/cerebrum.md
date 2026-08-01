# Cerebrum

> OpenWolf's learning memory. Updated automatically as the AI learns from interactions.
> Do not edit manually unless correcting an error.
> Last updated: 2026-08-01

## User Preferences

- 用中文交流；代码注释也用中文。
- 每个 `unsafe` 块上方必须有 `// SAFETY:` 注释说明前提，`unsafe fn` 要写 `# Safety` 文档段。
- 写新模块时对照 `linux/` 下的原始 C 代码，并在文档注释里点明对应的原版文件/函数。

## Key Learnings

- **Project:** shitix — 用 Rust 重写 Linux 1.0.9，x86_64，QEMU。
- 引导链是自己写的：`bootsect.S` → `setup.S` → `head.S` → `call start_kernel`，内核编成 **staticlib** 由 `ld` 链接，**不用 bootimage/bootloader crate**。
- `head.S` 用 SysV ABI 把 `0x90000` 放进 `rdi` 传给 `start_kernel`，所以入口签名是 `extern "C" fn(*const BootParams) -> !`。
- edition 2024：`#[no_mangle]` 要写成 `#[unsafe(no_mangle)]`；直接借用 `static mut` 被禁，用 `core::ptr::addr_of_mut!`。
- `test.sh` 靠串口里的 `SHITIX_BOOT_OK` 判定成功；内核以 `hlt` 空转，所以正常路径必然被 `timeout` 杀掉（退出码 124 视为正常）。
- QEMU 挂盘用 `if=ide`（不是 floppy），`-m 256M`。
- 工具链固定 nightly（`rust-toolchain.toml`），依赖 `x86_64` crate（`--no-default-features --features instructions,const_fn`）。
- **E820 的 usable 区间包含低 1MB 里的启动期结构**（0x70000 页表、0x90000 参数区、0x9E000 E820 数组），不能直接全部当空闲内存——这是 bug-001 的根源。
- setup.S 只恒等映射了低 1GB，所以 mm 必须把可管理内存 clamp 到 `1<<30`；给 QEMU `-m 3G` 也只认 1GB。
- 启动期诊断信息用 `kprintln!`（同时写 VGA 和串口），这样 `test.sh` 的日志能抓到布尔断言，不必靠截屏 OCR。
- 验证内核逻辑的有效手法：写一段临时自检/压力测试跑一遍看串口输出，通过后删掉（比只读代码可靠）。
- QEMU 因内核 `hlt` 空转而不退出，管道里接 `grep` 会因缓冲拿不到输出——要用 `-serial file:xxx.log` 落盘再 grep。
- 查三重错误用 `qemu ... -d cpu_reset`，能直接看到 CPU Reset 与当时的寄存器状态。
- 验证 VGA 输出的方法：`(sleep 6; echo "screendump /tmp/x.ppm"; sleep 2; echo quit) | qemu ... -monitor stdio`，然后用 PIL 按 9x16 单元格逐行统计像素颜色。文本模式实际分辨率是 720x400。

## Do-Not-Repeat

<!-- Mistakes made and corrected. Each entry prevents the same mistake recurring. -->
<!-- Format: [YYYY-MM-DD] Description of what went wrong and what to do instead. -->
- [2026-08-02] 用 `if=floppy` 手工启动镜像 → 根本没引导，截屏只有 BIOS 的 "no bootable device"。手工跑 QEMU 时要照抄 `scripts/test.sh` 里的 `QEMU_BASE`（`if=ide`）。
- [2026-08-02] `screendump` 紧跟启动就发 → 抓到的是内核跑之前的空屏。必须先 `sleep` 几秒等启动完成。
- [2026-08-02] 把 E820 的 usable 区间原样灌进空闲页链表 → 第一次 `get_free_page` 就派出 0x70000 的页表，清零时三重错误。任何物理内存分配器都必须先保留低 1MB。详见 buglog bug-001。
- [2026-08-02] `mem_map` 表体紧贴内核镜像（0x48000）向后放会盖住 0x70000 的页表；表基址要 `.max(0x100000)`。

## Decision Log

<!-- Significant technical decisions with rationale. Why X was chosen over Y. -->
- [2026-08-02] **不引入 `bootloader_api`**，尽管用户最初要求。原因：该 crate 的 `entry_point!` 依赖 `bootloader` crate 把内核当 ELF 加载并传入 `BootInfo`，而本项目有自建引导链且内核是 staticlib；且它提供的是线性 framebuffer，拿不到 0xB8000 文本模式。保留现有 `extern "C" start_kernel(*const BootParams)` ABI，它承担同样职责。
- [2026-08-02] 控制台走 `fmt::Write` + `format_args!`，而不是继续手写 `print_dec` 之类的辅助函数：能直接用 `{}`/`{:#04x}` 格式化，且不需要堆分配。`serial.rs` 暂时保留 `print_dec`，因为测试脚本解析的串口输出格式不宜变动。
- [2026-08-02] 颜色属性存在 `Writer` 里而非每次调用传参；`cprint!` 通过「存旧值→输出→恢复」实现，这样 `print!` 保持简洁，panic handler 又能整段染色。
- [2026-08-02] **上 nightly + 用 `x86_64` crate**（用户拍板）。之前悬而未决的两个问题就此关闭，不再手写 IDT/PIC 结构体。
- [2026-08-02] MM 只保留原版真正的机制（`mem_map` 引用计数、空闲链表指针存在空闲页内、kmalloc 的 order 分档 + 魔数校验、整页空闲则归还），跳过原版为 386 时代所做的妥协（`secondary_page_list` 备用池、`wp_works_ok` 探测、`try_to_free_page` 换页）。后两者要等块设备和进程到位。
- [2026-08-02] `kmalloc` 的 `block_header` 不用裸 union（原版用 union 省 4 字节），改成两个字段，头从 8B 变 24B，档位大小按 64 位重算成 32/64/128/256/512/1024/2048/4080。可读性优先于省几字节。
- [2026-08-02] `unmap_page` 只清 PTE，不回收中间级页表（PD/PT）——与原版一致。自检里那 2 页差值是预期占用，已在输出里注明 `expect 2` 免得被当成泄漏。
