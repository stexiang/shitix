//! 设备驱动。对应 linux-1.0.9 的 `drivers/` 目录。
//!
//! 只移植了跑起一个根文件系统 + 一个控制台所必需的部分：
//!
//! | 本模块 | 原版 | 说明 |
//! |---|---|---|
//! | [`block::ll_rw`] | `drivers/block/ll_rw_blk.c` | 请求队列与 `ll_rw_block` |
//! | [`block::ramdisk`] | `drivers/block/ramdisk.c` | 内存盘，当 ROOT_DEV |
//! | [`char_dev::tty`] | `drivers/char/tty_io.c` | tty 队列与行规则（大幅裁剪）|
//! | [`char_dev::console`] | `drivers/char/console.c` | tty 到 VGA 的输出端 |
//! | [`char_dev::keyboard`] | `drivers/char/keyboard.c` | IRQ1 扫描码 → ASCII |
//! | [`char_dev::mem`] | `drivers/char/mem.c` | `/dev/null` `/dev/zero` `/dev/mem` |
//!
//! 没有移植：软驱（`floppy.c`，1800 行状态机，QEMU 里 ramdisk 够用）、
//! IDE 硬盘（`hd.c`，同理）、SCSI、光驱、串口 tty（`serial.c`，我们的
//! `src/serial.rs` 已经是个单向输出端）、打印机、鼠标、声卡。
//! `genhd.c` 的分区表解析也没有：ramdisk 没有分区。

pub mod block;
pub mod char_dev;

/// 初始化所有驱动。对应原版 `init/main.c` 里那串 `*_init()` 调用，
/// 以及 `blk_dev_init()`/`chr_dev_init()`。
///
/// # Safety
/// 启动期调用一次，需在 `fs::init()` 之后（驱动要注册进设备表）、
/// `mount_root()` 之前（根设备要先能读）。
pub unsafe fn init() {
    // SAFETY: 契约转交。顺序同原版 blk_dev_init：请求队列先清空，
    // 再让各驱动注册自己的 request_fn。
    unsafe {
        block::ll_rw::init();
        block::ramdisk::init();
        char_dev::init();
    }
}
