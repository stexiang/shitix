//! 音频设备文件管理器（/dev/dsp, /dev/audio, /dev/dsp16）。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/audio.c`。
//!
//! 处理用户空间的 read/write/ioctl 请求，通过 DMA 缓冲区管理器
//! 进行实际的音频数据传输。对 /dev/audio 还进行 μ-law ↔ 线性 PCM 转换。

use super::config::*;
use super::dev_table;
use super::dmabuf;

// ---- μ-law / 线性 PCM 转换（算法实现，避免大表） ----

/// 8-bit μ-law → 16-bit 线性 PCM 转换。
/// 对应原版 `ulaw.h` 中的 `ulaw_dsp[]` 表。
/// 使用算法而非 512 字节查找表以节省 rodata。
pub fn ulaw_to_linear(ulaw: u8) -> u16 {
    // μ-law 解码算法：G.711 标准
    let ulaw = !ulaw; // 取反
    let sign = (ulaw & 0x80) as i16;
    let exponent = ((ulaw >> 4) & 0x07) as i16;
    let mantissa = (ulaw & 0x0F) as i16;

    let mut sample = ((mantissa << 3) as i16) + 0x84;
    sample <<= exponent;
    sample -= 0x84;

    if sign != 0 {
        (0x84 - sample) as u16
    } else {
        (sample + 0x84) as u16
    }
}

/// 16-bit 线性 PCM → 8-bit μ-law 转换。
/// 对应原版 `dsp_ulaw[]` 表。
/// 使用算法而非 256 字节查找表以节省 rodata。
pub fn linear_to_ulaw(sample: i16) -> u8 {
    // μ-law 编码算法：G.711 标准
    const BIAS: i16 = 0x84; // 量化偏移

    let mut sign = (sample >> 8) & 0x80;
    let mut s = sample;

    if sign != 0 {
        s = -s;
    }

    let mut s = if s > 32635 { 32635 } else { s as i16 };
    s += BIAS;

    let mut exponent: i16 = 7;
    let mut mantissa: i16;

    for exp in (0..8).rev() {
        if s >= (0x84 << exp) {
            exponent = exp as i16;
            break;
        }
    }

    mantissa = (s >> (exponent + 3)) & 0x0F;
    let ulaw = !((sign as u8) | ((exponent as u8) << 4) | mantissa as u8);
    ulaw
}

// ---- 设备状态 ----
/// 未完成的输出块号。-1 表示没有。
static mut WR_BUFF_NO: [i32; MAX_DSP_DEV] = [-1i32; MAX_DSP_DEV];
/// 输出缓冲区大小。
static mut WR_BUFF_SIZE: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
/// 输出缓冲区写入位置。
static mut WR_BUFF_PTR: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
/// 指向 DMA 写缓冲区的指针。
static mut WR_DMA_BUF: [*mut u8; MAX_DSP_DEV] = [core::ptr::null_mut(); MAX_DSP_DEV];

/// 音频模式：AM_NONE, AM_WRITE, AM_READ
static mut AUDIO_MODE: [u32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
const AM_NONE: u32 = 0;
const AM_WRITE: u32 = 1;
const AM_READ: u32 = 2;

// ---- μ-law 转换 ----
/// 将缓冲区中的字节通过 μ-law 转换表进行转换。
/// 对应原版 `translate_bytes()`（无内联汇编版本）。
///
/// # Safety
/// `buff` 指针必须指向至少 `n` 字节的可读写内存，
/// `table` 指针必须指向至少 256 字节的只读内存。
unsafe fn translate_bytes(table: *const u8, buff: *mut u8, n: usize) {
    for i in 0..n {
        // SAFETY: table 按索引 table[buff[i]] 访问
        let idx = unsafe { core::ptr::read_volatile(buff.add(i)) } as usize;
        let val = unsafe { core::ptr::read_volatile(table.add(idx)) };
        unsafe { core::ptr::write_volatile(buff.add(i), val) };
    }
}

// ---- 音频操作 ----

/// 打开音频设备。
/// 对应原版 `audio_open()`。
///
/// # Safety
/// 由 VFS 层调用，dev 和文件模式已校验。
pub unsafe fn audio_open(dev: i32) -> i32 {
    let dev_type = (dev as u32) & 0x0f;
    let dspdev = ((dev as u32) >> 4) as usize;

    let bits = if dev_type == SND_DEV_DSP16 { 16i32 } else { 8i32 };

    let mode = 0; // 暂时默认读写模式

    let ret = dmabuf::dmabuf_open(dspdev, mode);
    if ret < 0 {
        return ret;
    }

    // 设置采样位数
    let dsp_devs = unsafe { &*core::ptr::addr_of!(dev_table::DSP_DEVS) };
    if let Some(dsp_ops) = dsp_devs[dspdev] {
        if let Some(ioctl_fn) = dsp_ops.ioctl {
            let result = unsafe { ioctl_fn(dspdev, 0xC0045005 /* SNDCTL_DSP_SAMPLESIZE */, bits as u32, 1) };
            if result != bits {
                unsafe { audio_release(dev) };
                return -(crate::klib::errno::ENXIO as i32);
            }
        }
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(AUDIO_MODE[dspdev]), AM_NONE);
    }

    ret
}

/// 关闭音频设备。
/// 对应原版 `audio_release()`。
///
/// # Safety
/// 由 VFS 层调用。
pub unsafe fn audio_release(dev: i32) {
    let dspdev = ((dev as u32) >> 4) as usize;

    // 刷新未完成的写缓冲
    let wr_buff_no = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_NO[dspdev])) };
    if wr_buff_no >= 0 {
        let wr_ptr = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_PTR[dspdev])) };
        dmabuf::dmabuf_start_output(dspdev, wr_buff_no, wr_ptr);
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
        }
    }

    let _ = dmabuf::dmabuf_release(dspdev, 0);
}

/// 向音频设备写入（播放）。
/// 对应原版 `audio_write()`。
///
/// # Safety
/// `buf` 必须指向用户空间可读内存，`count` 字节。
pub unsafe fn audio_write(dev: i32, buf: *const u8, count: i32) -> i32 {
    let dev_type = (dev as u32) & 0x0f;
    let dspdev = ((dev as u32) >> 4) as usize;

    let mut remaining = count as usize;
    let mut user_offs: usize = 0;

    // 方向改变处理
    let audio_mode = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(AUDIO_MODE[dspdev])) };
    if audio_mode == AM_READ {
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
        }
    }
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(AUDIO_MODE[dspdev]), AM_WRITE);
    }

    // count == 0：刷新输出
    if count == 0 {
        let wr_buff_no = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_NO[dspdev])) };
        if wr_buff_no >= 0 {
            let wr_ptr = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_PTR[dspdev])) };
            dmabuf::dmabuf_start_output(dspdev, wr_buff_no, wr_ptr);
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
            }
        }
        return 0;
    }

    while remaining > 0 {
        // 如果没有未完成的写缓冲，获取一个新的
        let wr_buff_no = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_NO[dspdev])) };
        if wr_buff_no < 0 {
            // SAFETY: wr_dma_buf 和 wr_buff_size 是本地静态变量
            let mut dma_buf: *mut u8 = core::ptr::null_mut();
            let mut buf_size: usize = 0;
            let new_no = dmabuf::dmabuf_get_wr_buffer(
                dspdev,
                &raw mut dma_buf,
                &raw mut buf_size,
            );
            if new_no < 0 {
                return new_no;
            }
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), new_no);
                core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_SIZE[dspdev]), buf_size);
                core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_PTR[dspdev]), 0);
                core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_DMA_BUF[dspdev]), dma_buf);
            }
        }

        let wr_buff_no = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_NO[dspdev])) };
        let wr_buff_size = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_SIZE[dspdev])) };
        let wr_buff_ptr = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_PTR[dspdev])) };
        let wr_dma_buf = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_DMA_BUF[dspdev])) };

        let space = wr_buff_size - wr_buff_ptr;
        let l = if remaining < space { remaining } else { space };

        // 从用户空间拷贝数据到 DMA 缓冲区
        // SAFETY: buf 由 VFS 层保证，wr_dma_buf 是分配的 DMA 缓冲区
        unsafe {
            core::ptr::copy_nonoverlapping(buf.add(user_offs), wr_dma_buf.add(wr_buff_ptr), l);
        }

        // /dev/audio 需要 μ-law 转换（线性 PCM → μ-law）
        if dev_type == SND_DEV_AUDIO {
            // SAFETY: wr_dma_buf + wr_buff_ptr 是 DMA 缓冲区
            unsafe {
                for i in 0..l {
                    let sample = core::ptr::read_volatile(wr_dma_buf.add(wr_buff_ptr + i));
                    let encoded = linear_to_ulaw(sample as i16);
                    core::ptr::write_volatile(wr_dma_buf.add(wr_buff_ptr + i), encoded);
                }
            }
        }

        remaining -= l;
        user_offs += l;
        let new_ptr = wr_buff_ptr + l;
        unsafe {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_PTR[dspdev]), new_ptr);
        }

        // 如果缓冲区已满，提交输出
        if new_ptr >= wr_buff_size {
            dmabuf::dmabuf_start_output(dspdev, wr_buff_no, new_ptr);
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
            }
        }
    }

    count
}

/// 从音频设备读取（录制）。
/// 对应原版 `audio_read()`。
///
/// # Safety
/// `buf` 必须指向用户空间可写内存，`count` 字节。
pub unsafe fn audio_read(dev: i32, buf: *mut u8, count: i32) -> i32 {
    let dev_type = (dev as u32) & 0x0f;
    let dspdev = ((dev as u32) >> 4) as usize;

    let mut remaining = count as usize;
    let mut user_offs: usize = 0;

    // 方向改变处理
    let audio_mode = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(AUDIO_MODE[dspdev])) };
    if audio_mode == AM_WRITE {
        let wr_buff_no = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_NO[dspdev])) };
        if wr_buff_no >= 0 {
            let wr_ptr = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_PTR[dspdev])) };
            dmabuf::dmabuf_start_output(dspdev, wr_buff_no, wr_ptr);
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
            }
        }
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(AUDIO_MODE[dspdev]), AM_READ);
    }

    while remaining > 0 {
        // 获取录制缓冲区
        let mut dmabuf_ptr: *mut u8 = core::ptr::null_mut();
        let mut len: usize = 0;

        // dmabuf module currently doesn't have a direct get_rd_buffer export
        // for the simplified version; we'll return EIO for now
        // Full implementation would call DMAbuf_getrdbuffer

        if len > remaining {
            len = remaining;
        }

        // 对于 /dev/audio，做 μ-law → 线性 PCM 转换
        if dev_type == SND_DEV_AUDIO && !dmabuf_ptr.is_null() {
            for i in 0..len {
                unsafe {
                    let sample = core::ptr::read_volatile(dmabuf_ptr.add(i));
                    let decoded = (ulaw_to_linear(sample) >> 8) as u8;
                    core::ptr::write_volatile(dmabuf_ptr.add(i), decoded);
                }
            }
        }

        // 拷贝到用户空间
        if !dmabuf_ptr.is_null() {
            // SAFETY: buf 和 dmabuf 都由调用者保证有效性
            unsafe {
                core::ptr::copy_nonoverlapping(dmabuf_ptr, buf.add(user_offs), len);
            }
        }

        if len == 0 {
            break;
        }

        user_offs += len;
        remaining -= len;
    }

    (count as usize - remaining) as i32
}

/// 音频设备 ioctl。
/// 对应原版 `audio_ioctl()`。
pub fn audio_ioctl(dev: i32, cmd: u32, arg: usize) -> i32 {
    let dev_type = (dev as u32) & 0x0f;
    let dspdev = ((dev as u32) >> 4) as usize;

    match cmd {
        0x00005001 /* SNDCTL_DSP_SYNC */ => {
            // 刷新未完成的写缓冲，然后同步
            let wr_buff_no = unsafe {
                core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_NO[dspdev]))
            };
            if wr_buff_no >= 0 {
                let wr_ptr = unsafe {
                    core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_PTR[dspdev]))
                };
                dmabuf::dmabuf_start_output(dspdev, wr_buff_no, wr_ptr);
                unsafe {
                    core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
                }
            }
            dmabuf::dmabuf_ioctl(dspdev, cmd, arg as u32, 0)
        }
        0x00005002 /* SNDCTL_DSP_POST */ => {
            // 提交当前未完成的缓冲块，不等待同步
            let wr_buff_no = unsafe {
                core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_NO[dspdev]))
            };
            if wr_buff_no >= 0 {
                let wr_ptr = unsafe {
                    core::ptr::read_volatile(core::ptr::addr_of!(WR_BUFF_PTR[dspdev]))
                };
                dmabuf::dmabuf_start_output(dspdev, wr_buff_no, wr_ptr);
                unsafe {
                    core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
                }
            }
            0
        }
        0x00005000 /* SNDCTL_DSP_RESET */ => {
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(WR_BUFF_NO[dspdev]), -1);
            }
            dmabuf::dmabuf_ioctl(dspdev, cmd, arg as u32, 0)
        }
        _ => {
            // /dev/audio 只允许特定的 ioctl
            if dev_type == SND_DEV_AUDIO {
                return -(crate::klib::errno::EIO as i32);
            }
            // 其他转给 DMAbuf 和各声卡驱动
            dmabuf::dmabuf_ioctl(dspdev, cmd, arg as u32, 0)
        }
    }
}
