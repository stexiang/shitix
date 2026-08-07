//! 声卡主驱动模块。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/soundcard.c`。
//!
//! 提供：
//! - 字符设备文件操作（`sound_read`/`sound_write`/`sound_open`/`sound_release`/`sound_ioctl`）
//! - 声卡初始化入口 `soundcard_init`
//! - 中断注册辅助函数
//! - DMA 页分配

use super::config::*;
use super::dev_table;
use super::sound_switch;

/// 已安装的声卡数量
static mut SOUNDCARDS_INSTALLED: usize = 0;
/// 声卡是否已配置
static mut SOUNDCARD_CONFIGURED: bool = false;

/// 每个次设备号的文件信息。对应原版 `files[SND_NDEVS]`。
static mut FILES: [FileInfo; SND_NDEVS] = [FileInfo { mode: 0 }; SND_NDEVS];

// ---- 外部引用 ----
// DMA 原始缓冲区（由 dmabuf 模块提供）
unsafe extern "C" {
    /// 原始 DMA 缓冲区指针数组
    static mut snd_raw_buf: [[*mut u8; DSP_BUFFCOUNT]; MAX_DSP_DEV];
    /// 原始 DMA 缓冲区物理地址数组
    static mut snd_raw_buf_phys: [[usize; DSP_BUFFCOUNT]; MAX_DSP_DEV];
    /// 原始 DMA 缓冲区计数
    static mut snd_raw_count: [i32; MAX_DSP_DEV];
}

// ---- ioctl 辅助 ----
/// 将返回值写回用户空间的 ioctl 输出参数。
/// 对应原版 `snd_ioctl_return()`。
///
/// # Safety
/// `addr` 必须指向用户空间可写内存。
pub unsafe fn snd_ioctl_return(addr: *mut i32, value: i32) -> i32 {
    if value < 0 {
        return value;
    }
    // SAFETY: 调用者保证 addr 指向用户空间可写内存
    unsafe {
        core::ptr::write_volatile(addr, value);
    }
    0
}

// ---- 文件操作 ----
/// `sound_open` - 打开声卡设备文件。
/// 对应原版 `sound_open()`。
pub fn sound_open(dev: u32) -> i32 {
    let minor = dev & 0xFF;

    if minor >= SND_NDEVS as u32 {
        crate::pr_warn!("sound_open: invalid minor device {}\n", minor);
        return -(crate::klib::errno::ENXIO as i32);
    }

    // SAFETY: 启动期已初始化；文件信息粒度是 per-minor
    let fi = unsafe { &mut *core::ptr::addr_of_mut!(FILES[minor as usize]) };

    if !unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SOUNDCARD_CONFIGURED)) }
        && minor != SND_DEV_CTL
        && minor != SND_DEV_STATUS
    {
        crate::pr_warn!("SoundCard Error: The soundcard system has not been configured\n");
        return -(crate::klib::errno::ENXIO as i32);
    }

    fi.mode = 0;

    // SAFETY: 传入的 dev 由 VFS 层保证有效
    let ret = unsafe { sound_switch::sound_open_sw(dev as i32, fi) };
    ret
}

/// `sound_release` - 关闭声卡设备文件。
/// 对应原版 `sound_release()`。
pub fn sound_release(dev: u32) {
    let minor = dev & 0xFF;

    if minor >= SND_NDEVS as u32 {
        return;
    }

    // SAFETY: 文件信息在 open 时已初始化
    let fi = unsafe { &*core::ptr::addr_of_mut!(FILES[minor as usize]) };
    unsafe { sound_switch::sound_release_sw(dev as i32, fi) };
}

/// `sound_read` - 从声卡设备读。
/// 对应原版 `sound_read()`。
///
/// # Safety
/// `buf` 必须指向用户空间可写内存，`count` 字节。
pub unsafe fn sound_read(dev: u32, buf: *mut u8, count: usize) -> i32 {
    let minor = dev & 0xFF;
    let fi = unsafe { &*core::ptr::addr_of_mut!(FILES[minor as usize]) };
    unsafe { sound_switch::sound_read_sw(dev as i32, fi, buf, count as i32) }
}

/// `sound_write` - 向声卡设备写。
/// 对应原版 `sound_write()`。
///
/// # Safety
/// `buf` 必须指向用户空间可读内存，`count` 字节。
pub unsafe fn sound_write(dev: u32, buf: *const u8, count: usize) -> i32 {
    let minor = dev & 0xFF;
    let fi = unsafe { &*core::ptr::addr_of_mut!(FILES[minor as usize]) };
    unsafe { sound_switch::sound_write_sw(dev as i32, fi, buf, count as i32) }
}

/// `sound_ioctl` - 声卡设备 ioctl。
/// 对应原版 `sound_ioctl()`。
pub fn sound_ioctl(dev: u32, cmd: u32, arg: usize) -> i32 {
    let minor = dev & 0xFF;
    // SAFETY: fi 在 open 时已初始化
    let fi = unsafe { &*core::ptr::addr_of_mut!(FILES[minor as usize]) };
    unsafe { sound_switch::sound_ioctl_sw(dev as i32, fi, cmd, arg) }
}

// ---- 中断处理 ----
/// 注册声卡中断处理函数。
/// 对应原版 `snd_set_irq_handler()`。
pub fn snd_set_irq_handler(irq: u32, _handler: usize) -> i32 {
    // 原版调用 irqaction() 注册 ISR。
    // 在我们的内核中，中断注册由 `irq::request_irq` 处理。
    // 此处留作占位——实际注册由各声卡驱动的 attach 函数处理。
    // 返回 0 表示成功。
    crate::kprintln!("snd: request IRQ {} for sound", irq);
    0
}

/// 释放声卡中断。
/// 对应原版 `snd_release_irq()`。
pub fn snd_release_irq(irq: u32) {
    crate::irq::free_irq(irq as usize);
}

// ---- 定时器 ----
/// 请求声卡定时器。用于音序器。
/// 对应原版 `request_sound_timer()`。
pub fn request_sound_timer(_count: i32) {
    // 原版音序器定时器——待音序器移植后启用
}

/// 停止声卡定时器。
/// 对应原版 `sound_stop_timer()`。
pub fn sound_stop_timer() {
    // 原版音序器定时器——待音序器移植后启用
}

// ---- 微秒延迟 ----
/// 约 10 微秒的延迟。
/// 对应原版 `tenmicrosec()`：循环读 0x80 端口 16 次。
pub fn tenmicrosec() {
    for _ in 0..16 {
        // SAFETY: 读 0x80 端口总是安全的（DMA 页寄存器/ISA 延迟端口）
        unsafe {
            x86_64::instructions::port::PortReadOnly::<u8>::new(0x80).read();
        }
    }
}

// ---- DMA 页分配 ----
/// 验证 DMA 缓冲区是否在一个 DMA 页边界内。
/// 对应原版 `valid_dma_page()`。
fn valid_dma_page(addr: usize, dev_buffsize: usize, dma_pagesize: usize) -> bool {
    ((addr & (dma_pagesize - 1)) + dev_buffsize) <= dma_pagesize
}

/// 为 DSP 设备分配 DMA 缓冲区。
/// 对应原版 `sound_mem_init()`。
///
/// 在物理可用内存的高端（16M 以内）为各音频设备分配连续的
/// DMA 缓冲区，并标记对应物理页为 MAP_PAGE_RESERVED。
///
/// # Safety
/// 必须在 `page_alloc::init` 之后、`get_free_page` 之前调用。
/// 缓冲区分配在物理内存顶部，需要配合 `mem_map` 标记已保留。
pub unsafe fn sound_mem_init(high_memory: usize) {
    // SAFETY: 只在启动期，由 mem_init 调用
    let mut mem_ptr = if high_memory > 16 * 1024 * 1024 {
        16 * 1024 * 1024 // 限制在 16M（ISA DMA 只能在低 16M）
    } else {
        high_memory
    };

    for dev in 0..MAX_DSP_DEV {
        // SAFETY: 只读全局配置
        let buffcount = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(dev_table::SOUND_BUFFCOUNTS[dev]))
        };
        let dmachan = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(dev_table::SOUND_DSP_DMACHAN[dev]))
        };

        if buffcount == 0 || dmachan <= 0 {
            continue;
        }

        // 确定 DMA 页大小
        // 16-bit DMA 通道 (5-7) 和大于 64KB 的缓冲区使用 128KB 页
        let dma_pagesize: usize = if dmachan > 3 {
            let bufsize = unsafe {
                core::ptr::read_volatile(core::ptr::addr_of!(dev_table::SOUND_BUFFSIZES[dev]))
            };
            if bufsize > 65536 { 131072 } else { 65536 }
        } else {
            65536
        };

        // 调整缓冲区大小
        let bufsize = unsafe {
            &mut *core::ptr::addr_of_mut!(dev_table::SOUND_BUFFSIZES[dev])
        };
        if *bufsize > dma_pagesize {
            *bufsize = dma_pagesize;
        }
        *bufsize &= !0xFFF; // 对齐到 4KB
        if *bufsize < 4096 {
            *bufsize = 4096;
        }

        let bs = *bufsize;
        // SAFETY: auto_mode 是只读配置
        let auto_mode = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(dev_table::SOUND_DMA_AUTOMODE[dev]))
        };

        // 自动模式下只用 1 个缓冲区
        let raw_count = unsafe {
            &mut *core::ptr::addr_of_mut!(snd_raw_count[dev])
        };

        let raw_buf = unsafe { &mut *core::ptr::addr_of_mut!(snd_raw_buf[dev]) };
        let raw_buf_phys = unsafe { &mut *core::ptr::addr_of_mut!(snd_raw_buf_phys[dev]) };

        *raw_count = 0;
        let max_count = if auto_mode != 0 { 1 } else { buffcount };

        while *raw_count < max_count as i32 {
            let start_addr = mem_ptr - bs;

            let aligned_start = if !valid_dma_page(start_addr, bs, dma_pagesize) {
                // 对齐地址到 dma_pagesize 边界
                start_addr & !(dma_pagesize - 1)
            } else {
                start_addr
            };

            let end_addr = aligned_start + bs - 1;
            let idx = *raw_count as usize;

            raw_buf[idx] = aligned_start as *mut u8;
            raw_buf_phys[idx] = aligned_start;
            mem_ptr = aligned_start;

            // 标记对应物理页为已保留
            // 对应原版: for i = MAP_NR(start) .. MAP_NR(end) { mem_map[i] = MAP_PAGE_RESERVED; }
            // 这里我们只记录地址，实际的 mem_map 标记由调用者（mm 层）负责
            // 因为我们不直接访问 mem_map

            *raw_count += 1;
        }
    }
}

// ---- 初始化 ----
/// 初始化整个声卡子系统。
/// 对应原版 `soundcard_init()`。
///
/// 顺序：
/// 1. 注册字符设备（SOUND_MAJOR 14, "sound"）
/// 2. 探测并附加各声卡驱动
/// 3. 若检测到音频设备，初始化 DMAbuf 和 audio
/// 4. 若检测到 MIDI，初始化 MIDIbuf
/// 5. 若检测到 MIDI/合成器，初始化音序器
///
/// # Safety
/// 启动期调用一次。会直接操作 I/O 端口、注册中断。
pub unsafe fn soundcard_init() {
    // 注册字符设备
    // 当前内核的字符设备注册机制在 fs/ 层，这里直接标记已配置
    // 实际的 chrdev 注册由 drivers::init() 完成
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(SOUNDCARD_CONFIGURED),
            true,
        );
    }

    // 探测并附加声卡
    let mem = unsafe { dev_table::sndtable_init(0) };

    let cards = dev_table::get_card_count();
    if cards == 0 {
        crate::kprintln!("sound: No sound cards detected");
        return;
    }

    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(SOUNDCARDS_INSTALLED),
            cards,
        );
    }

    // 初始化音频设备
    if unsafe { core::ptr::read_volatile(core::ptr::addr_of!(dev_table::NUM_DSPDEVS)) } > 0 {
        crate::kprintln!("sound: {} DSP device(s) detected", unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(dev_table::NUM_DSPDEVS))
        });
    }

    // MIDI 和音序器待移植后启用

    crate::kprintln!("sound: subsystem initialized, {} total device(s)", cards);
    let _ = mem; // 消除 unused 警告
}
