# STATUS — shitix

> Single source of truth for resuming work. Read this FIRST when starting a session.
> Update this file at the end of every work phase so the next `/clear` resumes in 1 read.
> Last updated: 2026-08-02 (模块 2 完成)

---

## ✅ Done

**引导链（64 位）**
- `bootsect.S` 512B 引导扇区，INT 13h/42h LBA 读盘，自搬到 0x9000
- `setup.S` 机器参数 + E820 探测、开 A20、建 4 级页表（恒等映射低 1GB / 2MB 大页）、切 long mode
- `head.S` 装 GDT/IDT（256 门 → `ignore_int`）、16KB 内核栈、清 BSS、`call start_kernel`

**模块 1：内核入口 + VGA 文本输出**（2026-08-02）
- `src/lib.rs`：`start_kernel(*const BootParams) -> !`，打印 e820 条目数 / 可用 RAM / 显示模式 / 光标位置；`#[panic_handler]` 白字红底 + 位置信息 + 串口 `SHITIX_PANIC`
- `src/console.rs`：`Color`(16 色) / `ColorCode` / `Writer`(impl `fmt::Write`)，滚屏、`\n\r\t\b`、0x3D4/0x3D5 硬件光标；导出 `print!` `println!` `cprint!` `cprintln!`
- 验证：debug + release 均 PASS；`screendump` 确认 7 行文本、4 种颜色（cyan/gray/green/yellow）、光标下划线、panic 红底渲染正常

**模块 2：内存管理**（2026-08-02，对应原版 `mm/`）
- `src/mm/page.rs`：`PAGE_*` 常量、`page_align`、`map_nr`（原版 `page.h`）
- `src/mm/page_alloc.rs`：页帧分配器（原版 `mem_init` + `__get_free_page`/`free_page`）。`mem_map` u16 引用计数 + `MAP_PAGE_RESERVED`，空闲链表指针存在空闲页头 8 字节；低 1MB 整体保留
- `src/mm/kmalloc.rs`：八档小块分配器（原版 `kmalloc.c`），`PageDescriptor`/`BlockHeader`/`MF_USED`/`MF_FREE`，整页空闲则归还页帧分配器
- `src/mm/paging.rs`：四级页表 `map_page`/`map_range`/`translate`/`unmap_page` + `flags`（原版 `put_page`/`remap_page_range`/`invalidate`），能识别 setup.S 的 2MB 大页
- `src/e820.rs`：把 setup.S 的 E820 表包成迭代器喂给 mm
- `src/serial.rs`：加 `fmt::Write`，新增 `sprint!`/`sprintln!`/`kprintln!`
- 工具链：`rust-toolchain.toml` 固定 nightly，加 `x86_64` 0.15 依赖
- 验证：`mm_selftest()` 三条路径全绿（页分配去重+清零、kmalloc 6 档读写、映射别名+translate+unmap）；临时压力测试确认超大 kmalloc 返回 null、double free 只计一次、保留页释放是 noop、耗尽 65214 页后可全量回收；`-m 32M/128M/1G/3G` 与 release 均 PASS；零警告
- 踩坑：低 1MB 的页表被当空闲页派出去导致三重错误，见 buglog bug-001

---

## 🚀 Next phase

**Goal:** 移植中断与异常处理 —— 把 `head.S` 里全指向 `ignore_int` 的 IDT 换成 Rust 侧真正的处理函数（对应 linux-1.0.9 `kernel/traps.c` 的 `trap_init()`）。

### Acceptance criteria
1. 除零 / 无效 opcode / page fault 等异常能打印向量号、错误码、`rip`，而不是三重错误重启
2. 8259A PIC 重映射到向量 0x20-0x2F，timer(IRQ0) 中断能被计数
3. `sti` 之后内核能持续空转不崩

### Files to create / edit
| Type | File | Content |
|---|---|---|
| new | `src/idt.rs` | 64 位 IDT 表 + `IdtEntry`，取代 head.S 的 `setup_idt` |
| new | `src/interrupts.rs` | 异常/中断处理函数，`extern "x86-interrupt"` |
| new | `src/pic.rs` | 8259A 初始化与 EOI（参考 `linux/kernel/irq.c`）|
| edit | `boot/head.S` | 保留早期 `ignore_int` 兜底，IDT 交给 Rust 重装 |
| edit | `src/lib.rs` | `start_kernel` 里调用 `idt::init()` / `pic::init()` |

### Closed decisions
- 沿用现有 `extern "C" start_kernel` ABI，不迁移到 `bootloader_api`（理由见 cerebrum Decision Log）
- **用 nightly + `x86_64` crate**（用户 2026-08-02 拍板）：`rust-toolchain.toml` 已固定 nightly，`x86_64` 0.15 已在依赖里，IDT/PIC 用 crate 提供的 `InterruptDescriptorTable` 而非手写结构体
- 中断里若要分配内存，可直接用已完成的 `mm::kmalloc`/`get_free_page`；注意原版在中断上下文强制 `GFP_ATOMIC`，我们目前没有 `secondary_page_list` 备用池，中断里分配失败要能容忍

### Open decisions
- 无（可直接开工）

---

## 📁 Active architecture

- **Stack:** Rust `#![no_std]` + edition 2024 + **nightly**（`rust-toolchain.toml` 固定），crate-type = `staticlib`，依赖 `x86_64` 0.15；GNU as + ld；QEMU x86_64
- **Key modules:** `src/lib.rs`(入口) / `src/console.rs`(VGA) / `src/serial.rs`(COM1) / `src/e820.rs` / `src/mm/`(page, page_alloc, kmalloc, paging) / `boot/*.S` + `boot/*.ld`
- **物理内存约定:** 低 1MB 永久保留（启动期结构）；`mem_map` 放 0x100000 起；可管理内存 clamp 到 1GB（setup.S 的恒等映射上限）
- **Patterns:**
  - 每个 `unsafe` 块上方写 `// SAFETY:`；`unsafe fn` 写 `# Safety` 文档段
  - 新模块的文档注释里注明对应的 linux-1.0.9 原版文件/函数
  - MMIO 一律 `read_volatile` / `write_volatile`
  - 全局可变状态用 `addr_of_mut!` 访问（edition 2024 禁止直接借用 `static mut`）
  - 注释与交流用中文

---

## ⚠️ External blockers (don't block coding)

- 无。工具链（cargo / as / ld / qemu-system-x86_64 / python3+PIL）本机齐备

---

## 🔧 Useful commands

```bash
scripts/test.sh                  # 构建 + 无头启动，匹配 SHITIX_BOOT_OK
scripts/test.sh run              # 带窗口交互运行
scripts/test.sh debug            # 等 GDB: gdb -ex 'target remote :1234' target/boot/system.elf
PROFILE=release scripts/test.sh  # release 构建并测试
scripts/build.sh                 # 只出镜像 target/boot/shitix.img

# 验证 VGA 实际渲染（QEMU 文本模式为 720x400，单元格 9x16）
(sleep 6; echo "screendump /tmp/x.ppm"; sleep 2; echo quit) | \
  qemu-system-x86_64 -drive format=raw,file=target/boot/shitix.img,if=ide \
  -m 256M -no-reboot -display none -serial null -monitor stdio

# 跨内存规格验证（注意：内核 hlt 不退出，必须落盘再 grep，不能直接管道接 grep）
for m in 32M 128M 1G 3G; do
  timeout 12 qemu-system-x86_64 -drive format=raw,file=target/boot/shitix.img,if=ide \
    -m $m -no-reboot -display none -serial file:/tmp/m_$m.log -monitor none >/dev/null 2>&1
  echo "=== $m ==="; grep -aE "^mm: [0-9]|held=|BOOT_OK|PANIC" /tmp/m_$m.log
done

# 查三重错误 / 异常重启
qemu-system-x86_64 ... -d cpu_reset
```

---

## 📚 References (read IF needed)

- `.wolf/cerebrum.md` — User Preferences + Do-Not-Repeat + Decision Log
- `.wolf/anatomy.md` — token-efficient file index
- `.wolf/buglog.json` — known bugs + fixes
