//! 声卡设备分派层。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/sound_switch.c`。
//!
//! 根据次设备号（`dev & 0x0f`）将 VFS 的 `read`/`write`/`open`/`release`/`ioctl`
//! 分派到对应的音频/混音器/音序器/MIDI 处理函数。

use super::config::*;
use super::dev_table;

// ---- 设备引用计数 ----
#[derive(Clone, Copy)]
struct SbcDevice {
    usecount: i32,
}

const fn default_sbc_devices() -> [SbcDevice; SND_NDEVS] {
    [SbcDevice { usecount: 0 }; SND_NDEVS]
}

static mut SBC_DEVICES: [SbcDevice; SND_NDEVS] = default_sbc_devices();
static mut IN_USE: i32 = 0; // 总打开设备数（不含 minor 0 / CTL）

// ---- /dev/sndstatus ----
static mut STATUS_BUF: *mut u8 = core::ptr::null_mut();
static mut STATUS_LEN: usize = 0;
static mut STATUS_PTR: usize = 0;
static mut STATUS_BUSY: bool = false;

// ---- 分派函数 ----

/// 根据次设备号分派 `read`。
/// 对应原版 `sound_read_sw()`。
///
/// # Safety
/// `buf` 必须指向用户空间可写内存。
pub unsafe fn sound_read_sw(dev: i32, _file: &FileInfo, buf: *mut u8, count: i32) -> i32 {
    let minor = (dev as u32) & 0x0f;

    match minor {
        SND_DEV_STATUS => {
            // SAFETY: 只读状态缓冲区
            unsafe { read_status(buf, count as usize) }
        }
        SND_DEV_DSP | SND_DEV_DSP16 | SND_DEV_AUDIO => {
            // 转给音频模块处理
            // SAFETY: 由 VFS 层保证 buf 有效性
            unsafe { super::audio::audio_read(dev, buf, count) }
        }
        _ => {
            crate::pr_warn!("Sound: read on unsupported minor device {}\n", minor);
            -(crate::klib::errno::EPERM as i32)
        }
    }
}

/// 根据次设备号分派 `write`。
/// 对应原版 `sound_write_sw()`。
///
/// # Safety
/// `buf` 必须指向用户空间可读内存。
pub unsafe fn sound_write_sw(dev: i32, _file: &FileInfo, buf: *const u8, count: i32) -> i32 {
    let minor = (dev as u32) & 0x0f;

    match minor {
        SND_DEV_DSP | SND_DEV_DSP16 | SND_DEV_AUDIO => {
            // SAFETY: 由 VFS 层保证 buf 有效性
            unsafe { super::audio::audio_write(dev, buf, count) }
        }
        _ => {
            crate::pr_warn!("Sound: write on unsupported minor device {}\n", minor);
            -(crate::klib::errno::EPERM as i32)
        }
    }
}

/// 根据次设备号分派 `open`。
/// 对应原版 `sound_open_sw()`。
///
/// # Safety
/// 由 VFS 层调用。
pub unsafe fn sound_open_sw(dev: i32, _file: &FileInfo) -> i32 {
    let minor = (dev as u32) & 0x0f;

    if minor as usize >= SND_NDEVS {
        crate::pr_warn!("Invalid minor device {}\n", minor);
        return -(crate::klib::errno::ENXIO as i32);
    }

    let retval = match minor {
        SND_DEV_STATUS => {
            // SAFETY: 单线程访问
            let busy = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STATUS_BUSY)) };
            if busy {
                return -(crate::klib::errno::EBUSY as i32);
            }
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_BUSY), true);
                // 分配 4000 字节给状态信息
                let page_addr = crate::mm::page_alloc::get_free_page();
                let page = if page_addr != 0 { page_addr as *mut u8 } else { core::ptr::null_mut() };
                if page.is_null() {
                    core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_BUSY), false);
                    return -(crate::klib::errno::EIO as i32);
                }
                core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_BUF), page);
                core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_LEN), 0usize);
                core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_PTR), 0usize);
                init_status();
            }
            0
        }
        SND_DEV_CTL => 0, // 混音器控制端口——总是允许打开

        SND_DEV_DSP | SND_DEV_DSP16 | SND_DEV_AUDIO => {
            // SAFETY: audio 模块的 open 路径
            unsafe { super::audio::audio_open(dev) }
        }

        _ => {
            crate::pr_warn!("Sound: open on unsupported minor device {}\n", minor);
            -(crate::klib::errno::ENXIO as i32)
        }
    };

    if retval >= 0 {
        // SAFETY: 单线程
        unsafe {
            let devices = core::ptr::addr_of_mut!(SBC_DEVICES);
            (*devices)[minor as usize].usecount += 1;
            let in_use = core::ptr::addr_of_mut!(IN_USE);
            core::ptr::write_volatile(in_use, core::ptr::read_volatile(in_use) + 1);
        }
    }

    retval
}

/// 根据次设备号分派 `release`。
/// 对应原版 `sound_release_sw()`。
///
/// # Safety
/// 由 VFS 层调用。
pub unsafe fn sound_release_sw(dev: i32, _file: &FileInfo) {
    let minor = (dev as u32) & 0x0f;

    match minor {
        SND_DEV_STATUS => {
            let status_buf = unsafe {
                core::ptr::read_volatile(core::ptr::addr_of!(STATUS_BUF))
            };
            if !status_buf.is_null() {
                // SAFETY: 之前由 get_free_page 分配
                unsafe {
                    crate::mm::page_alloc::free_page(status_buf as usize);
                }
            }
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_BUF), core::ptr::null_mut());
                core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_BUSY), false);
            }
        }
        SND_DEV_CTL => {} // 无事可做

        SND_DEV_DSP | SND_DEV_DSP16 | SND_DEV_AUDIO => {
            // SAFETY: audio 模块的 release 路径
            unsafe { super::audio::audio_release(dev) };
        }

        _ => {
            crate::pr_warn!("Sound: releasing unknown device 0x{:02x}\n", minor);
        }
    }

    // SAFETY: 单线程
    unsafe {
        let devices = core::ptr::addr_of_mut!(SBC_DEVICES);
        if (*devices)[minor as usize].usecount > 0 {
            (*devices)[minor as usize].usecount -= 1;
        }
        let in_use = core::ptr::addr_of_mut!(IN_USE);
        let cur = core::ptr::read_volatile(in_use);
        if cur > 0 {
            core::ptr::write_volatile(in_use, cur - 1);
        }
    }
}

/// 根据次设备号分派 `ioctl`。
/// 对应原版 `sound_ioctl_sw()`。
///
/// # Safety
/// `arg` 为通用指针，含义取决于 cmd。
pub unsafe fn sound_ioctl_sw(dev: i32, _file: &FileInfo, cmd: u32, arg: usize) -> i32 {
    let minor = (dev as u32) & 0x0f;

    match minor {
        SND_DEV_CTL => {
            // 混音器的次设备号在高 4 位
            let mixer_dev_id = ((dev as u32) >> 4) as usize;
            let num_mixers = unsafe {
                core::ptr::read_volatile(core::ptr::addr_of!(dev_table::NUM_MIXERS))
            };
            if num_mixers == 0 {
                return -(crate::klib::errno::ENXIO as i32);
            }
            if mixer_dev_id >= num_mixers {
                return -(crate::klib::errno::ENXIO as i32);
            }
            // SAFETY: mixer_dev_id 已校验
            let mixer_devs = unsafe {
                &*core::ptr::addr_of!(dev_table::MIXER_DEVS)
            };
            if let Some(mixer_ops) = mixer_devs[mixer_dev_id] {
                if let Some(ioctl_fn) = mixer_ops.ioctl {
                    // SAFETY: ioctl_fn 是声卡驱动提供的函数
                    return unsafe { ioctl_fn(mixer_dev_id, cmd, arg as u32) };
                }
            }
            -(crate::klib::errno::EIO as i32)
        }
        SND_DEV_DSP | SND_DEV_DSP16 | SND_DEV_AUDIO => {
            // SAFETY: audio 模块的 ioctl 路径
            unsafe { super::audio::audio_ioctl(dev, cmd, arg) }
        }
        _ => {
            crate::pr_warn!("Sound: ioctl on unsupported minor device {}\n", minor);
            -(crate::klib::errno::EPERM as i32)
        }
    }
}

// ---- 状态输出 ----
/// 输出字符串到状态缓冲区。
///
/// # Safety
/// 由 `init_status` 在持有 STATUS_BUF 时调用。
unsafe fn put_status(s: &str) -> bool {
    let l = s.len();
    let status_len = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STATUS_LEN)) };
    if status_len + l >= 4000 {
        return false;
    }
    // SAFETY: STATUS_BUF 在 open 时分配
    let buf = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STATUS_BUF)) };
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), buf.add(status_len), l);
    }
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_LEN), status_len + l);
    }
    true
}

/// 输出整数到状态缓冲区。
///
/// # Safety
/// 同 `put_status`。
unsafe fn put_status_int(val: u32, radix: u32) -> bool {
    if val == 0 {
        return unsafe { put_status("0") };
    }

    let hex = b"0123456789abcdef";
    let mut buf: [u8; 11] = [0; 11];
    let mut v = val;
    let mut l: usize = 0;

    while v > 0 && l < 10 {
        buf[9 - l] = hex[(v % radix) as usize];
        v /= radix;
        l += 1;
    }

    let status_len = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STATUS_LEN)) };
    if status_len + l >= 4000 {
        return false;
    }

    // SAFETY: STATUS_BUF 在 open 时分配
    let sbuf = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STATUS_BUF)) };
    unsafe {
        core::ptr::copy_nonoverlapping(buf[10 - l..].as_ptr(), sbuf.add(status_len), l);
    }
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_LEN), status_len + l);
    }
    true
}

/// 初始化状态信息字符串。
/// 对应原版 `init_status()`。
///
/// # Safety
/// 在 `sound_open_sw(SND_DEV_STATUS)` 内，`STATUS_BUF` 已分配。
unsafe fn init_status() {
    unsafe {
        let _ = put_status("Sound Driver: shitix-oss 1.0.9-rs\n");
        let _ = put_status("Config options: 0x");
        let _ = put_status_int(0, 16);
        let _ = put_status("\n\nHW config:\n");

        // 遍历已注册的声卡驱动
        for i in 0..dev_table::NUM_SOUND_DRIVERS.saturating_sub(1) {
            let driver = &*core::ptr::addr_of!(dev_table::SUPPORTED_DRIVERS[i]);
            if !driver.enabled {
                let _ = put_status("(");
            }
            let _ = put_status("Type ");
            let _ = put_status_int(driver.card_type as u32, 10);
            let _ = put_status(": ");
            let _ = put_status(driver.name);
            let _ = put_status(" at 0x");
            let _ = put_status_int(driver.config.io_base as u32, 16);
            let _ = put_status(" irq ");
            let _ = put_status_int(driver.config.irq as u32, 10);
            let _ = put_status(" drq ");
            let _ = put_status_int(driver.config.dma as u32, 10);
            if !driver.enabled {
                let _ = put_status(")");
            }
            let _ = put_status("\n");
        }
    }
}

/// 读取状态信息到用户缓冲区。
/// 对应原版 `read_status()`。
///
/// # Safety
/// `buf` 必须指向用户空间可写内存。
unsafe fn read_status(buf: *mut u8, count: usize) -> i32 {
    let status_len = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STATUS_LEN)) };
    let status_ptr = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STATUS_PTR)) };

    let remaining = status_len.saturating_sub(status_ptr);
    let l = if count < remaining { count } else { remaining };
    if l == 0 {
        return 0;
    }

    // SAFETY: STATUS_BUF 已分配，buf 由 VFS 层保证
    let sbuf = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STATUS_BUF)) };
    unsafe {
        core::ptr::copy_nonoverlapping(sbuf.add(status_ptr), buf, l);
    }
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(STATUS_PTR), status_ptr + l);
    }

    l as i32
}
