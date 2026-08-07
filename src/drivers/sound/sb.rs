//! SoundBlaster 声卡驱动（含 SB 1.0/1.5/2.0, SB Pro, SB16）。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/sb_card.c` + `sb_dsp.c` +
//! `sb_mixer.c` + `sb_midi.c` + `sb.h` + `sb_mixer.h`。
//!
//! 驱动 SoundBlaster 系列的 DSP 芯片（数字音频录制/回放）、
//! 混音器（音量控制）以及 OPL-2/OPL-3 FM 合成器。

use super::config::*;
use super::dev_table;
use super::dmabuf;
use super::opl3;

// ---- 全局状态 ----
/// SoundBlaster 基地址
static mut SBC_BASE_VAR: u16 = SBC_BASE;
/// SoundBlaster IRQ
static mut SBC_IRQ_VAR: u8 = SBC_IRQ;
/// DSP 版本：主版本号
static mut DSP_MAJOR: i32 = 1;
/// DSP 版本：次版本号
static mut DSP_MINOR: i32 = 0;
/// DSP 是否已成功初始化
static mut SB_DSP_OK: i32 = 0;
/// SB16 标志
static mut SB16: i32 = 0;
/// DSP 型号：1=SB, 2=SB Pro
static mut SB_DSP_MODEL: i32 = 1;
/// 是否支持高速模式
static mut SB_DSP_HIGHSPEED: i32 = 0;
/// 当前采样率
static mut DSP_CURRENT_SPEED: u32 = DSP_DEFAULT_SPEED;
/// 立体声标志
static mut DSP_STEREO: i32 = 0;
/// 中断已验证
static mut IRQ_VERIFIED: i32 = 0;
/// 设备索引
static mut MY_DEV: usize = 0;
/// MIDI 是否禁用
static mut MIDI_DISABLED: i32 = 0;

/// 混音器型号
static mut MIXER_MODEL: i32 = 0;
/// 混音器是否已初始化
static mut MIXER_INITIALIZED: bool = false;

/// 中断模式：IMODE_NONE, IMODE_OUTPUT, IMODE_INPUT
const IMODE_NONE: i32 = 0;
const IMODE_OUTPUT: i32 = 1;
const IMODE_INPUT: i32 = 2;

/// 当前中断模式
static mut SB_IRQ_MODE: i32 = IMODE_NONE;
/// 中断响应 OK 标记
static mut IRQ_OK: i32 = 0;

/// MIDI 模式：NORMAL_MIDI
const NORMAL_MIDI: i32 = 0;

// ---- DSP 命令 ----
/// 向 DSP 发送命令字节。
/// 对应原版 `sb_dsp_command()`。
///
/// 轮询 DSP 状态端口，等待 DSP 准备好接收命令（bit7=0），
/// 超时约 0.1 秒。
///
/// # Safety
/// `sbc_base` 必须是有效的 SoundBlaster 基地址。
unsafe fn sb_dsp_command(val: u8) -> bool {
    // SAFETY: 调用者保证 sbc_base 有效
    let dsp_status_port = unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(SBC_BASE_VAR))
    } + DSP_WRITE;

    let mut port = x86_64::instructions::port::PortReadOnly::<u8>::new(dsp_status_port);

    // 超时循环：约 500000 次迭代
    for _ in 0..500000 {
        // SAFETY: I/O 端口读
        let status = unsafe { port.read() };
        if (status & 0x80) == 0 {
            // SAFETY: I/O 端口写
            let mut cmd_port = x86_64::instructions::port::PortWriteOnly::<u8>::new(dsp_status_port);
            unsafe { cmd_port.write(val) };
            return true;
        }
    }

    crate::pr_warn!("SoundBlaster: DSP command 0x{:02x} timed out\n", val);
    false
}

/// 读取 DSP 数据字节。
///
/// # Safety
/// `sbc_base` 必须是有效的 SoundBlaster 基地址。
unsafe fn sb_dsp_read() -> u8 {
    let dsp_read_port = unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(SBC_BASE_VAR))
    } + DSP_READ;

    // 等待数据就绪（bit7=1）
    let mut status_port = x86_64::instructions::port::PortReadOnly::<u8>::new(
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SBC_BASE_VAR)) } + DSP_STATUS
    );

    for _ in 0..500000 {
        // SAFETY: I/O 端口读
        let status = unsafe { status_port.read() };
        if (status & 0x80) != 0 {
            let mut data_port = x86_64::instructions::port::PortReadOnly::<u8>::new(dsp_read_port);
            return unsafe { data_port.read() };
        }
    }

    0xFF // 超时返回
}

// ---- DSP 复位 ----
/// 复位 SoundBlaster DSP。
/// 对应原版 `sb_reset_dsp()`。
///
/// # Safety
/// `sbc_base` 必须是有效的 SoundBlaster 基地址。
pub unsafe fn sb_reset_dsp() -> bool {
    let sbc_base = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SBC_BASE_VAR)) };

    // 步骤 1：向 DSP_RESET 写 1，等待 3μs，再写 0
    // SAFETY: I/O 端口操作
    unsafe {
        let mut reset_port = x86_64::instructions::port::PortWriteOnly::<u8>::new(sbc_base + DSP_RESET);
        reset_port.write(1u8);
    }
    // 延迟约 3 微秒
    for _ in 0..100 {
        core::hint::spin_loop();
    }
    unsafe {
        let mut reset_port = x86_64::instructions::port::PortWriteOnly::<u8>::new(sbc_base + DSP_RESET);
        reset_port.write(0u8);
    }
    for _ in 0..100 {
        core::hint::spin_loop();
    }

    // 步骤 2：读 DSP 数据端口，等 0xAA 复位成功信号
    let dsp_read_port = sbc_base + DSP_READ;
    let dsp_status_port = sbc_base + DSP_STATUS;

    let mut read_port = x86_64::instructions::port::PortReadOnly::<u8>::new(dsp_read_port);
    let mut status_port = x86_64::instructions::port::PortReadOnly::<u8>::new(dsp_status_port);

    for _ in 0..100000 {
        let status = unsafe { status_port.read() };
        if (status & 0x80) != 0 {
            let data = unsafe { read_port.read() };
            if data == 0xAA {
                return true;
            }
        }
    }

    false
}

// ---- 采样率设置 ----
/// 计算 SoundBlaster 的采样率常数值。
/// 对应原版 `dsp_speed()`。
fn dsp_speed(speed: u32) -> u32 {
    let max_speed = if unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SB_DSP_HIGHSPEED)) } != 0 {
        44100
    } else {
        22050
    };

    if speed > max_speed {
        return max_speed;
    }
    if speed < 4000 {
        return 4000;
    }

    // SoundBlaster 使用 time_constant = 256 - (1000000 / speed)
    256 - (1000000 / speed)
}

// ---- 检测 ----
/// 检测 SoundBlaster DSP 芯片。
/// 对应原版 `sb_dsp_detect()`。
///
/// # Safety
/// `hw_config` 给出的 I/O 地址必须有效。
pub unsafe fn sb_dsp_detect(hw_config: &AddressInfo) -> bool {
    let sbc_base = hw_config.io_base;

    if sbc_base == 0 {
        return false;
    }

    // 保存当前的基地址
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SBC_BASE_VAR), sbc_base);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SBC_IRQ_VAR), hw_config.irq);
    }

    // 复位 DSP 并检查 0xAA 回应
    // SAFETY: sbc_base 由调用者保证有效
    unsafe { sb_reset_dsp() }
}

// ---- 混音器操作 ----
/// 写 SoundBlaster 混音器寄存器。
/// 对应原版 `sb_setmixer()`。
///
/// # Safety
/// `sbc_base` 必须是有效的 SoundBlaster 基地址。
unsafe fn sb_setmixer(port: u32, value: u32) {
    let sbc_base = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SBC_BASE_VAR)) };
    // SAFETY: I/O 端口操作，在中断禁用时调用
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(sbc_base + MIXER_ADDR)
            .write((port & 0xFF) as u8);
        super::soundcard::tenmicrosec();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(sbc_base + MIXER_DATA)
            .write((value & 0xFF) as u8);
        super::soundcard::tenmicrosec();
    }
}

/// 读 SoundBlaster 混音器寄存器。
/// 对应原版 `sb_getmixer()`。
///
/// # Safety
/// `sbc_base` 必须是有效的 SoundBlaster 基地址。
unsafe fn sb_getmixer(port: u32) -> u32 {
    let sbc_base = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SBC_BASE_VAR)) };
    // SAFETY: I/O 端口操作
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(sbc_base + MIXER_ADDR)
            .write((port & 0xFF) as u8);
        super::soundcard::tenmicrosec();
        let val = x86_64::instructions::port::PortReadOnly::<u8>::new(sbc_base + MIXER_DATA).read();
        super::soundcard::tenmicrosec();
        val as u32
    }
}

// ---- 中断处理 ----
/// SoundBlaster 中断处理函数。
/// 对应原版 DSP 输出/输入完成中断。
///
/// # Safety
/// 由中断框架调用，在中断上下文中。
pub unsafe fn sb_dsp_interrupt() {
    let irq_mode = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SB_IRQ_MODE)) };

    // 标记中断已收到
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(IRQ_OK), 1);
    }

    let dev = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(MY_DEV)) };

    match irq_mode {
        IMODE_OUTPUT => {
            dmabuf::dmabuf_output_intr(dev, 0);
        }
        IMODE_INPUT => {
            // 输入完成处理——暂未实现
        }
        _ => {}
    }
}

// ---- DSP 操作接口 ----

/// 打开 SoundBlaster DSP。
fn sb_dsp_open(dev: usize, mode: u32) -> i32 {
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(MY_DEV), dev);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SB_IRQ_MODE), IMODE_NONE);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(IRQ_OK), 0);
    }
    0
}

/// 关闭 SoundBlaster DSP。
fn sb_dsp_close(dev: usize) {
    // SAEEFY: 停止 DMA 传输
    unsafe {
        // 复位 DSP
        let _ = sb_reset_dsp();
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SB_IRQ_MODE), IMODE_NONE);
        // 重编程默认采样率
        let speed = DSP_DEFAULT_SPEED;
        let tc = dsp_speed(speed);
        sb_dsp_command(0x40); // Set time constant
        sb_dsp_command(tc as u8);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DSP_CURRENT_SPEED), speed);
    }
    let _ = dev;
}

/// 输出音频块。
fn sb_dsp_output_block(dev: usize, buf_phys: usize, count: usize, intrflag: i32, dma_restart: i32) {
    let sbc_base = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SBC_BASE_VAR)) };

    // 如果是首次启动，需要编程 DMA 控制器
    if dma_restart != 0 {
        let dma_chan = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(dev_table::SOUND_DSP_DMACHAN[dev]))
        };
        // 编程 ISA DMA 控制器
        // SAFETY: DMA 通道号由配置表保证有效
        unsafe {
            // 设置 DMA 模式、地址、长度
            // 原版: disable_dma, clear_dma_ff, set_dma_mode, set_dma_addr, set_dma_count, enable_dma
            let dma_port = match dma_chan {
                0 => 0x00,
                1 => 0x02,
                2 => 0x04,
                3 => 0x06,
                5 => 0xC4, // 16-bit DMA
                6 => 0xC8,
                7 => 0xCC,
                _ => return,
            };

            // 简化：对于 QEMU 环境（无真实 DMA），直接告诉 DSP 启动
            // 单周期 DMA 输出命令 0x14 = 8-bit PCM, 0xB0 = 16-bit PCM
            let is_16bit = core::ptr::read_volatile(core::ptr::addr_of!(SB16)) != 0;
            let cmd = if is_16bit { 0xB0u8 } else { 0x14u8 };
            let count_1 = (count - 1) as u8;

            // 设置时间常数（首次）
            let speed = core::ptr::read_volatile(core::ptr::addr_of!(DSP_CURRENT_SPEED));
            if speed != DSP_DEFAULT_SPEED || dma_restart == 1 {
                let tc = dsp_speed(speed);
                sb_dsp_command(0x40);  // Set time constant
                sb_dsp_command(tc as u8);
            }

            // 告诉 DSP 开始播放
            sb_dsp_command(cmd);
            sb_dsp_command(count_1 & 0xFF);

            if is_16bit {
                sb_dsp_command(((count - 1) >> 8) as u8);
            }
        }
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SB_IRQ_MODE), IMODE_OUTPUT);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(IRQ_OK), 0);
    }
    let _ = (buf_phys, intrflag);
}

/// 复位 DSP 设备。
fn sb_dsp_reset(dev: usize) {
    let _ = dev;
    // SAFETY: 在持有设备锁时调用
    unsafe {
        let _ = sb_reset_dsp();
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SB_IRQ_MODE), IMODE_NONE);
    }
}

/// 停止传输。
fn sb_dsp_halt_xfer(dev: usize) {
    let _ = dev;
    unsafe {
        // 发送 DSP Halt 命令
        sb_dsp_command(0xD0); // Halt DMA operation
    }
}

/// SoundBlaster DSP 特有的 ioctl。
fn sb_dsp_ioctl(dev: usize, cmd: u32, arg: u32, _local: i32) -> i32 {
    match cmd {
        0xC0045004 /* SNDCTL_DSP_SPEED / SOUND_PCM_WRITE_RATE */ => {
            if arg > 0 {
                let tc = dsp_speed(arg);
                unsafe {
                    sb_dsp_command(0x40); // Set time constant
                    sb_dsp_command(tc as u8);
                    core::ptr::write_volatile(core::ptr::addr_of_mut!(DSP_CURRENT_SPEED), arg);
                }
            }
            unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DSP_CURRENT_SPEED)) as i32 }
        }
        0x80045004 /* SOUND_PCM_READ_RATE */ => {
            unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DSP_CURRENT_SPEED)) as i32 }
        }
        0xC0045006 /* SOUND_PCM_WRITE_CHANNELS */ => {
            if arg == 2 {
                unsafe {
                    core::ptr::write_volatile(core::ptr::addr_of_mut!(DSP_STEREO), 1);
                }
            } else {
                unsafe {
                    core::ptr::write_volatile(core::ptr::addr_of_mut!(DSP_STEREO), 0);
                }
            }
            arg as i32
        }
        0x80045006 /* SOUND_PCM_READ_CHANNELS */ => {
            let stereo = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(DSP_STEREO)) };
            if stereo != 0 { 2 } else { 1 }
        }
        0xC0045005 /* SOUND_PCM_WRITE_BITS */ => {
            let val = if arg == 16 { 16 } else { 8 };
            unsafe {
                sb_dsp_command(if val == 16 { 0xB0u8 } else { 0x14u8 });
            }
            val
        }
        0x80045005 /* SOUND_PCM_READ_BITS */ => {
            let is_16bit = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SB16)) } != 0;
            if is_16bit { 16 } else { 8 }
        }
        _ => {
            crate::pr_warn!("sb_dsp: unknown ioctl cmd=0x{:08x}\n", cmd);
            -(crate::klib::errno::EINVAL as i32)
        }
    }
}

// ---- 混音器 ioctl ----
/// SoundBlaster 混音器 ioctl。
fn sb_mixer_ioctl(dev: usize, cmd: u32, arg: u32) -> i32 {
    let _ = dev;

    match cmd {
        0xC0044D00 /* SOUND_MIXER_WRITE_VOLUME */ => {
            let vol = arg as u32;
            let left = vol & 0xFF;
            let right = (vol >> 8) & 0xFF;

            // 设置主音量
            unsafe {
                sb_setmixer(SB_MIXER_MASTER_VOL as u32, (left << 3) | (right >> 5));
            }
            vol as i32
        }
        0x80044D00 /* SOUND_MIXER_READ_VOLUME */ => {
            let val = unsafe { sb_getmixer(SB_MIXER_MASTER_VOL as u32) };
            ((val & 0xF8) | ((val >> 3) & 0xF8) << 8) as i32
        }
        _ => {
            -(crate::klib::errno::EINVAL as i32)
        }
    }
}

// ---- 静态虚表 ----
/// SoundBlaster DSP 操作表。对应原版 `audio_operations`。
static SB_DSP_OPS: dev_table::AudioOperations = dev_table::AudioOperations {
    name: "SoundBlaster",
    open: Some(sb_dsp_open),
    close: Some(sb_dsp_close),
    output_block: Some(sb_dsp_output_block),
    start_input: None,
    ioctl: Some(sb_dsp_ioctl),
    prepare_for_input: None,
    prepare_for_output: None,
    reset: Some(sb_dsp_reset),
    halt_xfer: Some(sb_dsp_halt_xfer),
    has_output_drained: None,
    copy_from_user: None,
};

/// SoundBlaster 混音器操作表。对应原版 `mixer_operations`。
static SB_MIXER_OPS: dev_table::MixerOperations = dev_table::MixerOperations {
    ioctl: Some(sb_mixer_ioctl),
};

// ---- 公共 API ----

/// 探测 SoundBlaster 声卡。
/// 对应原版 `probe_sb()`。
pub fn probe_sb(hw_config: &AddressInfo) -> bool {
    // SAFETY: hw_config 中的 I/O 地址由探测表保证
    unsafe { sb_dsp_detect(hw_config) }
}

/// 附加 SoundBlaster 声卡。
/// 对应原版 `attach_sb_card()`。
pub fn attach_sb(mem_start: usize, hw_config: &AddressInfo) -> usize {
    let mut mem = mem_start;

    // 探测 DSP
    if !probe_sb(hw_config) {
        return mem;
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SBC_BASE_VAR), hw_config.io_base);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SBC_IRQ_VAR), hw_config.irq);
    }

    // 初始化 DMA 通道配置
    if hw_config.dma > 0 {
        unsafe {
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(dev_table::SOUND_DSP_DMACHAN[0]),
                hw_config.dma as i32,
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(dev_table::SOUND_BUFFCOUNTS[0]),
                DSP_BUFFCOUNT,
            );
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(dev_table::SOUND_BUFFSIZES[0]),
                65536, // 64KB 缓冲区
            );
        }
    }

    // 尝试使用高速 DSP 模式
    let dsp_ver = unsafe {
        sb_dsp_command(0xE1); // Get DSP version
        let major = sb_dsp_read() as i32;
        let minor = sb_dsp_read() as i32;

        core::ptr::write_volatile(core::ptr::addr_of_mut!(DSP_MAJOR), major);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(DSP_MINOR), minor);

        // DSP v4.0+ (SB16) 支持高速和 16 位
        if major >= 4 {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(SB16), 1);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(SB_DSP_HIGHSPEED), 1);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(SB_DSP_MODEL), 4); // SB Pro+
        } else if major >= 3 {
            core::ptr::write_volatile(core::ptr::addr_of_mut!(SB_DSP_MODEL), 2); // SB Pro
        }
        (major, minor)
    };

    crate::kprintln!(
        "sb: DSP version {}.{} detected at 0x{:03x} irq {} dma {}",
        dsp_ver.0, dsp_ver.1,
        hw_config.io_base, hw_config.irq, hw_config.dma,
    );

    // 注册 DSP 音频设备
    unsafe {
        dev_table::register_audio_dev(&SB_DSP_OPS);
    }

    // 如果是 SB Pro 或以上，也注册混音器
    let dsp_model = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SB_DSP_MODEL)) };
    if dsp_model >= 2 {
        unsafe {
            dev_table::register_mixer_dev(&SB_MIXER_OPS);
            core::ptr::write_volatile(core::ptr::addr_of_mut!(MIXER_INITIALIZED), true);
        }

        // 初始化混音器：主音量默认
        unsafe {
            sb_setmixer(SB_MIXER_MASTER_VOL as u32, 0xDD); // 左右全音量
        }
    }

    // 检测 OPL-2/3 并初始化 FM 合成器
    // SAFETY: sbc_base 已确认有效
    if unsafe { opl3::opl3_detect(hw_config.io_base) } {
        mem = unsafe { opl3::opl3_init(mem) };
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(SB_DSP_OK), 1);
    }

    mem
}

/// 初始化 SoundBlaster 驱动在全局表中的条目。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn init_sb_driver() {
    let drivers = unsafe { &mut *core::ptr::addr_of_mut!(dev_table::SUPPORTED_DRIVERS) };
    for i in 0..dev_table::NUM_SOUND_DRIVERS.saturating_sub(1) {
        if drivers[i].card_type as u32 == SoundCardType::Sb as u32 {
            drivers[i].probe = Some(probe_sb);
            drivers[i].attach = Some(attach_sb);
            drivers[i].enabled = true;
            break;
        }
    }

    // 同样启用 SB16
    for i in 0..dev_table::NUM_SOUND_DRIVERS.saturating_sub(1) {
        if drivers[i].card_type as u32 == SoundCardType::Sb16 as u32 {
            drivers[i].probe = Some(probe_sb);
            drivers[i].attach = Some(attach_sb);
            drivers[i].enabled = true;
            break;
        }
    }
}
