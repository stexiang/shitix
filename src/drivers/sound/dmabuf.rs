//! DMA 缓冲区管理器。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/dmabuf.c`。
//!
//! 管理原始物理 DMA 缓冲区的分配、逻辑子缓冲区的拆分与环形队列，
//! 以及 DMA 传输的启动、中断处理、睡眠/唤醒。

use super::config::*;
use super::dev_table;

// ---- 原始 DMA 缓冲区 ----
/// 原始 DMA 缓冲区指针。对应原版 `snd_raw_buf[MAX_DSP_DEV][DSP_BUFFCOUNT]`。
#[unsafe(no_mangle)]
static mut snd_raw_buf: [[*mut u8; DSP_BUFFCOUNT]; MAX_DSP_DEV] =
    [[core::ptr::null_mut(); DSP_BUFFCOUNT]; MAX_DSP_DEV];

/// 原始 DMA 缓冲区物理地址。对应原版 `snd_raw_buf_phys[MAX_DSP_DEV][DSP_BUFFCOUNT]`。
#[unsafe(no_mangle)]
static mut snd_raw_buf_phys: [[usize; DSP_BUFFCOUNT]; MAX_DSP_DEV] =
    [[0usize; DSP_BUFFCOUNT]; MAX_DSP_DEV];

/// 原始 DMA 缓冲区数量。对应原版 `snd_raw_count[MAX_DSP_DEV]`。
#[unsafe(no_mangle)]
static mut snd_raw_count: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];

// ---- 设备状态表 ----
static mut DMA_MODE: [u32; MAX_DSP_DEV] = [DMODE_NONE; MAX_DSP_DEV];
static mut DMABUF_INTERRUPTED: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_BUSY: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_NEEDS_RESTART: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_MODES: [u32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_ACTIVE: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_STARTED: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_QLEN: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_QHEAD: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_QTAIL: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_UNDERRUN: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut BUFFERALLOC_DONE: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];

// ---- 逻辑缓冲区 ----
static mut DEV_NBUFS: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_COUNTS: [[usize; MAX_SUB_BUFFERS]; MAX_DSP_DEV] =
    [[0usize; MAX_SUB_BUFFERS]; MAX_DSP_DEV];
static mut DEV_SUBDIVISION: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
static mut DEV_BUF_PHYS: [[usize; MAX_SUB_BUFFERS]; MAX_DSP_DEV] =
    [[0usize; MAX_SUB_BUFFERS]; MAX_DSP_DEV];
static mut DEV_BUF: [[*mut u8; MAX_SUB_BUFFERS]; MAX_DSP_DEV] =
    [[core::ptr::null_mut(); MAX_SUB_BUFFERS]; MAX_DSP_DEV];
static mut DEV_BUFFSIZE: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];

// ---- 睡眠标志 ----
// 原版用 wait_queue + snd_wait 结构，这里简化：用简单的布尔标志 + 忙等待。
// 完整版需要接入调度器的 sleep_on/wake_up 机制。
static mut DEV_SLEEP_FLAG: [[u32; MAX_DSP_DEV]; 1] = [[0; MAX_DSP_DEV]];

/// 分析物理 DMA 缓冲区，拆分出逻辑子缓冲区。
/// 对应原版 `reorganize_buffers()`。
///
/// # Safety
/// 假设在持有设备锁时调用（中断禁用）。
unsafe fn reorganize_buffers(dev: usize) {
    // 从 DSP 设备读取当前 PCM 参数
    let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
    let dsp_ops = match dsp_devs[dev] {
        Some(ops) => ops,
        None => return,
    };

    let ioctl_fn = dsp_ops.ioctl.unwrap_or(|_, _, _, _| 0);

    // SAFETY: 调用声卡驱动的 ioctl
    let sr = unsafe { ioctl_fn(dev, 0x80045004 /* SOUND_PCM_READ_RATE */, 0, 1) } as u32;
    let nc = unsafe { ioctl_fn(dev, 0x80045006 /* SOUND_PCM_READ_CHANNELS */, 0, 1) } as u32;
    let sz = unsafe { ioctl_fn(dev, 0x80045005 /* SOUND_PCM_READ_BITS */, 0, 1) } as u32;

    let (sr_val, nc_val, sz_val) = if sr < 1 || nc < 1 || sz < 1 {
        crate::pr_warn!("SOUND: Invalid PCM parameters[{}] sr={}, nc={}, sz={}\n", dev, sr, nc, sz);
        (DSP_DEFAULT_SPEED, 1u32, 8u32)
    } else {
        (sr, nc, sz)
    };

    let bytes_per_sample = sz_val / 8;
    let bytes_per_second = sr_val * nc_val * bytes_per_sample;

    let bufsize = unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(dev_table::SOUND_BUFFSIZES[dev]))
    };
    let buffcount = unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(dev_table::SOUND_BUFFCOUNTS[dev]))
    };

    // 缓冲区大小不超过 1 秒
    let mut bsz = bufsize;
    while bsz > bytes_per_second as usize {
        bsz >>= 1;
    }

    // 单缓冲区时至少拆成 2 个
    if buffcount == 1 && bsz == bufsize {
        bsz >>= 1;
    }

    let subdiv = unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(DEV_SUBDIVISION[dev]))
    };
    let subdiv = if subdiv == 0 { 1 } else { subdiv };

    bsz /= subdiv;
    if bsz < 4096 {
        bsz = 4096;
    }

    // 控制子缓冲区总数不超过 MAX_SUB_BUFFERS
    let raw_count = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(snd_raw_count[dev])) };
    while (bufsize * buffcount) / bsz > MAX_SUB_BUFFERS {
        bsz <<= 1;
    }

    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_BUFFSIZE[dev]), bsz); }

    let mut n = 0;
    let raw_buf = unsafe { &*core::ptr::addr_of!(snd_raw_buf[dev]) };
    let raw_buf_phys = unsafe { &*core::ptr::addr_of!(snd_raw_buf_phys[dev]) };

    for i in 0..raw_count as usize {
        let mut p = 0;
        while (p + bsz) <= bufsize {
            unsafe {
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!(DEV_BUF[dev][n]),
                    raw_buf[i].add(p),
                );
                core::ptr::write_volatile(
                    core::ptr::addr_of_mut!(DEV_BUF_PHYS[dev][n]),
                    raw_buf_phys[i] + p,
                );
            }
            p += bsz;
            n += 1;
        }
    }

    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_NBUFS[dev]), n); }

    for i in 0..n {
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_COUNTS[dev][i]), 0);
        }
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(BUFFERALLOC_DONE[dev]), 1);
    }
}

/// 初始化设备 DMA 缓冲区。
fn dma_init_buffers(dev: usize) {
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_SLEEP_FLAG[0][dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_UNDERRUN[dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_BUSY[dev]), 1);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(BUFFERALLOC_DONE[dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_ACTIVE[dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QLEN[dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QTAIL[dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QHEAD[dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_NEEDS_RESTART[dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_STARTED[dev]), 0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DMA_MODE[dev]), DMODE_NONE);
    }
}

/// 打开 DMA 缓冲区设备。
/// 对应原版 `DMAbuf_open()`。
pub fn dmabuf_open(dev: usize, mode: u32) -> i32 {
    if dev >= MAX_DSP_DEV {
        return -(crate::klib::errno::ENXIO as i32);
    }

    // SAFETY: 启动期已初始化
    let busy = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_BUSY[dev])) };
    if busy != 0 {
        return -(crate::klib::errno::EBUSY as i32);
    }

    let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
    if dsp_devs[dev].is_none() {
        crate::pr_warn!("DSP device {} not initialized\n", dev);
        return -(crate::klib::errno::ENXIO as i32);
    }

    // 检查是否有分配的 DMA 缓冲区
    let raw_buf = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(snd_raw_buf[dev][0])) };
    if raw_buf.is_null() {
        return -(crate::klib::errno::ENOSPC as i32);
    }

    let dsp_ops = dsp_devs[dev].unwrap();

    // SAFETY: dsp_ops 已确认非空
    if let Some(open_fn) = dsp_ops.open {
        let ret = unsafe { open_fn(dev, mode) };
        if ret < 0 {
            return ret;
        }
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_MODES[dev]), mode);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_SUBDIVISION[dev]), 0);
    }

    dma_init_buffers(dev);

    // 设置默认 PCM 参数
    if let Some(ioctl_fn) = dsp_ops.ioctl {
        // SAFETY: 调用声卡驱动 ioctl
        unsafe {
            ioctl_fn(dev, 0xC0045005 /* SOUND_PCM_WRITE_BITS */, 8, 1);
            ioctl_fn(dev, 0xC0045006 /* SOUND_PCM_WRITE_CHANNELS */, 1, 1);
            ioctl_fn(dev, 0xC0045004 /* SOUND_PCM_WRITE_RATE */, DSP_DEFAULT_SPEED, 1);
        }
    }

    0
}

/// 复位 DMA 缓冲区。
/// 对应原版 `dma_reset()`。
///
/// # Safety
/// 应在禁用中断时调用。
unsafe fn dma_reset(dev: usize) {
    let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };

    if let Some(dsp_ops) = dsp_devs[dev] {
        if let Some(reset_fn) = dsp_ops.reset {
            // SAFETY: 声卡驱动函数
            unsafe { reset_fn(dev) };
        }
        if let Some(close_fn) = dsp_ops.close {
            // SAFETY: 声卡驱动函数
            unsafe { close_fn(dev) };
        }

        let mode = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_MODES[dev])) };
        if let Some(open_fn) = dsp_ops.open {
            let ret = unsafe { open_fn(dev, mode) };
            if ret < 0 {
                crate::pr_warn!("Sound: Reset failed - Can't reopen device\n");
                return;
            }
        }
    }

    dma_init_buffers(dev);
    // SAFETY: 中断已禁用
    unsafe { reorganize_buffers(dev) };
}

/// 同步 DMA 输出——等待所有缓冲数据播放完毕。
/// 对应原版 `dma_sync()`。
pub fn dma_sync(dev: usize) -> usize {
    let dma_mode = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DMA_MODE[dev])) };
    if dma_mode != DMODE_OUTPUT {
        return unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QLEN[dev])) };
    }

    // 等待队列排空——简化版本：对于无硬件 DMA 的环境，
    // 立即返回当前队列长度
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QLEN[dev])) }
}

/// 释放 DMA 缓冲区。
/// 对应原版 `DMAbuf_release()`。
pub fn dmabuf_release(dev: usize, _mode: u32) -> i32 {
    let dma_mode = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DMA_MODE[dev])) };

    if dma_mode == DMODE_OUTPUT {
        let _ = dma_sync(dev);
    }

    let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
    if let Some(dsp_ops) = dsp_devs[dev] {
        if let Some(reset_fn) = dsp_ops.reset {
            // SAFETY: 声卡驱动函数
            unsafe { reset_fn(dev) };
        }
        if let Some(close_fn) = dsp_ops.close {
            // SAFETY: 声卡驱动函数
            unsafe { close_fn(dev) };
        }
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DMA_MODE[dev]), DMODE_NONE);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_BUSY[dev]), 0);
    }

    0
}

/// 获取一个写缓冲区用于输出。
/// 对应原版 `DMAbuf_getwrbuffer()`。
pub fn dmabuf_get_wr_buffer(dev: usize, buf: *mut *mut u8, size: *mut usize) -> i32 {
    let dma_mode = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DMA_MODE[dev])) };

    // 方向变化处理
    if dma_mode == DMODE_INPUT {
        unsafe {
            dma_reset(dev);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DMA_MODE[dev]), DMODE_NONE);
        }
    } else {
        let needs_restart = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(DEV_NEEDS_RESTART[dev]))
        };
        if needs_restart != 0 {
            let _ = dma_sync(dev);
            unsafe { dma_reset(dev) };
        }
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_NEEDS_RESTART[dev]), 0);
    }

    // 按需重新组织缓冲区
    let bufferalloc_done = unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(BUFFERALLOC_DONE[dev]))
    };
    if bufferalloc_done == 0 {
        unsafe { reorganize_buffers(dev) };
    }

    // 初始化输出模式
    let curr_mode = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DMA_MODE[dev])) };
    if curr_mode == DMODE_NONE {
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DMA_MODE[dev]), DMODE_OUTPUT);
        }

        let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
        if let Some(dsp_ops) = dsp_devs[dev] {
            if let Some(prepare_fn) = dsp_ops.prepare_for_output {
                let bufsize = unsafe {
                    core::ptr::read_volatile(core::ptr::addr_of!(DEV_BUFFSIZE[dev]))
                };
                let nbufs = unsafe {
                    core::ptr::read_volatile(core::ptr::addr_of!(DEV_NBUFS[dev]))
                };
                let ret = unsafe { prepare_fn(dev, bufsize, nbufs) };
                if ret < 0 {
                    return ret;
                }
            }
        }
    }

    let qlen = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QLEN[dev])) };
    let nbufs = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_NBUFS[dev])) };

    if qlen >= nbufs {
        // 缓冲区已满，返回错误（原版会睡眠等待）
        return -(crate::klib::errno::EIO as i32);
    }

    let qtail = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QTAIL[dev])) };
    let dev_buf = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_BUF[dev][qtail])) };
    let bufsize = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_BUFFSIZE[dev])) };

    // SAFETY: buf 和 size 由调用者保证有效
    unsafe {
        core::ptr::write_volatile(buf, dev_buf);
        core::ptr::write_volatile(size, bufsize);
    }
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_COUNTS[dev][qtail]), 0);
    }

    qtail as i32
}

/// 开始输出：将填好的缓冲区提交给 DMA 引擎播放。
/// 对应原版 `DMAbuf_start_output()`。
pub fn dmabuf_start_output(dev: usize, buff_no: i32, l: usize) -> i32 {
    let qtail = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QTAIL[dev])) };

    if buff_no as usize != qtail {
        crate::pr_warn!("Soundcard warning: DMA buffers out of sync {} != {}\n", buff_no, qtail);
    }

    unsafe {
        let qlen = core::ptr::read_volatile(core::ptr::addr_of!(DEV_QLEN[dev]));
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QLEN[dev]), qlen + 1);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_COUNTS[dev][qtail]), l);
    }

    let bufsize = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_BUFFSIZE[dev])) };
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(DEV_NEEDS_RESTART[dev]),
            if l != bufsize { 1 } else { 0 },
        );
    }

    let nbufs = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_NBUFS[dev])) };
    let new_qtail = (qtail + 1) % nbufs;
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QTAIL[dev]), new_qtail);
    }

    let active = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_ACTIVE[dev])) };
    if active == 0 {
        let qhead = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QHEAD[dev])) };
        let buf_phys = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(DEV_BUF_PHYS[dev][qhead]))
        };
        let count = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(DEV_COUNTS[dev][qhead]))
        };

        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_ACTIVE[dev]), 1);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_STARTED[dev]), 1);
        }

        let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
        if let Some(dsp_ops) = dsp_devs[dev] {
            if let Some(output_fn) = dsp_ops.output_block {
                // SAFETY: 声卡驱动函数
                unsafe {
                    output_fn(
                        dev,
                        buf_phys,
                        count,
                        0, // intrflag: 不是来自中断
                        1, // dma_restart: 首次启动需要设置 DMA
                    );
                }
            }
        }
    }

    0
}

/// DMA 输出完成中断处理。
/// 对应原版 `DMAbuf_outputintr()`。
pub fn dmabuf_output_intr(dev: usize, underrun_flag: i32) {
    let nbufs = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_NBUFS[dev])) };
    let qlen = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QLEN[dev])) };
    let qhead = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QHEAD[dev])) };

    if qlen > 0 {
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QLEN[dev]), qlen - 1);
        }
    }

    let new_qhead = (qhead + 1) % nbufs;
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QHEAD[dev]), new_qhead);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_ACTIVE[dev]), 0);
    }

    let new_qlen = qlen.saturating_sub(1);
    if new_qlen > 0 {
        let qhead = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DEV_QHEAD[dev])) };
        let buf_phys = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(DEV_BUF_PHYS[dev][qhead]))
        };
        let count = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(DEV_COUNTS[dev][qhead]))
        };

        let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
        if let Some(dsp_ops) = dsp_devs[dev] {
            if let Some(output_fn) = dsp_ops.output_block {
                // SAFETY: 声卡驱动函数
                unsafe {
                    output_fn(
                        dev,
                        buf_phys,
                        count,
                        1, // intrflag: 来自中断
                        1,
                    );
                }
            }
        }
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_ACTIVE[dev]), 1);
        }
    } else if underrun_flag != 0 {
        unsafe {
            let u = core::ptr::read_volatile(core::ptr::addr_of!(DEV_UNDERRUN[dev]));
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_UNDERRUN[dev]), u + 1);
        }
        let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
        if let Some(dsp_ops) = dsp_devs[dev] {
            if let Some(halt_fn) = dsp_ops.halt_xfer {
                // SAFETY: 声卡驱动函数
                unsafe { halt_fn(dev) };
            }
        }
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_NEEDS_RESTART[dev]), 1);
        }
    }
}

/// DMA 缓冲区 ioctl。
/// 对应原版 `DMAbuf_ioctl()`。
pub fn dmabuf_ioctl(dev: usize, cmd: u32, arg: u32, _local: i32) -> i32 {
    match cmd {
        0x00005000 /* SNDCTL_DSP_RESET */ => {
            unsafe { dma_reset(dev) };
            0
        }
        0x00005001 /* SNDCTL_DSP_SYNC */ => {
            let _ = dma_sync(dev);
            unsafe { dma_reset(dev) };
            0
        }
        0xC0045002 /* SNDCTL_DSP_GETBLKSIZE */ => {
            let bufferalloc_done = unsafe {
                core::ptr::read_volatile(core::ptr::addr_of!(BUFFERALLOC_DONE[dev]))
            };
            if bufferalloc_done == 0 {
                unsafe { reorganize_buffers(dev) };
            }
            let bufsize = unsafe {
                core::ptr::read_volatile(core::ptr::addr_of!(DEV_BUFFSIZE[dev]))
            };
            // 将结果写回用户空间 ioctl 参数
            bufsize as i32
        }
        _ => {
            // 转给各声卡特定的 ioctl
            let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
            if let Some(dsp_ops) = dsp_devs[dev] {
                if let Some(ioctl_fn) = dsp_ops.ioctl {
                    // SAFETY: 声卡驱动函数
                    return unsafe { ioctl_fn(dev, cmd, arg, 0) };
                }
            }
            -(crate::klib::errno::EIO as i32)
        }
    }
}

/// 初始化 DMA 缓冲区管理器。
/// 对应原版 `DMAbuf_init()`。
pub fn dmabuf_init() {
    for i in 0..MAX_DSP_DEV {
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QLEN[i]), 0);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QHEAD[i]), 0);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_QTAIL[i]), 0);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_ACTIVE[i]), 0);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(DEV_BUSY[i]), 0);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(BUFFERALLOC_DONE[i]), 0);
        }
    }
}
