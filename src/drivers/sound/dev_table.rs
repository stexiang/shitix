//! 声卡设备调用表。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/dev_table.h` + `dev_table.c`。
//!
//! 定义了四类设备的虚函数表：audio_operations、mixer_operations、
//! synth_operations、midi_operations，以及全局注册表。

use super::config::*;

// ---- 音频操作虚表 ----
/// DSP / 音频设备操作接口。
/// 对应原版 `struct audio_operations`。
pub struct AudioOperations {
    /// 设备名称
    pub name: &'static str,
    /// 打开设备
    pub open: Option<fn(dev: usize, mode: u32) -> i32>,
    /// 关闭设备
    pub close: Option<fn(dev: usize)>,
    /// 提交输出块（DMA 传输）
    /// buf: 物理地址, count: 字节数, intrflag: 是否来自中断, dma_restart: 是否需要重启 DMA
    pub output_block: Option<fn(dev: usize, buf: usize, count: usize, intrflag: i32, dma_restart: i32)>,
    /// 启动输入（录制）
    pub start_input: Option<fn(dev: usize, buf: usize, count: usize, intrflag: i32, dma_restart: i32)>,
    /// 各设备特定 ioctl
    pub ioctl: Option<fn(dev: usize, cmd: u32, arg: u32, local: i32) -> i32>,
    /// 输入前准备
    pub prepare_for_input: Option<fn(dev: usize, bufsize: usize, nbufs: usize) -> i32>,
    /// 输出前准备
    pub prepare_for_output: Option<fn(dev: usize, bufsize: usize, nbufs: usize) -> i32>,
    /// 复位设备
    pub reset: Option<fn(dev: usize)>,
    /// 停止传输
    pub halt_xfer: Option<fn(dev: usize)>,
    /// 查询设备是否还有未播放的缓冲数据
    pub has_output_drained: Option<fn(dev: usize) -> bool>,
    /// 从用户空间拷贝数据到 DMA 缓冲区
    pub copy_from_user: Option<fn(dev: usize, local_buf: *mut u8, local_offs: usize, user_buf: *const u8, user_offs: usize, len: usize)>,
}

impl AudioOperations {
    /// 创建一个空的虚表（所有函数指针为 None）
    pub const fn empty() -> Self {
        AudioOperations {
            name: "",
            open: None,
            close: None,
            output_block: None,
            start_input: None,
            ioctl: None,
            prepare_for_input: None,
            prepare_for_output: None,
            reset: None,
            halt_xfer: None,
            has_output_drained: None,
            copy_from_user: None,
        }
    }
}

impl Default for AudioOperations {
    fn default() -> Self {
        Self::empty()
    }
}

// ---- 混音器操作虚表 ----
/// 混音器设备操作接口。
/// 对应原版 `struct mixer_operations`。
pub struct MixerOperations {
    /// 混音器 ioctl
    pub ioctl: Option<fn(dev: usize, cmd: u32, arg: u32) -> i32>,
}

impl Default for MixerOperations {
    fn default() -> Self {
        MixerOperations { ioctl: None }
    }
}

// ---- 合成器信息 ----
/// 合成器信息。
/// 对应原版 `struct synth_info`。
#[derive(Default)]
pub struct SynthInfo {
    /// 合成器名称
    pub name: &'static str,
    /// 合成器类型
    pub synth_type: i32,
    /// 合成器子类型
    pub synth_subtype: i32,
    /// 能力位
    pub capabilities: i32,
}

// ---- 合成器操作虚表 ----
/// 合成器设备操作接口。
/// 对应原版 `struct synth_operations`。
pub struct SynthOperations {
    /// 合成器信息
    pub info: &'static SynthInfo,
    /// 合成器类型
    pub synth_type: i32,
    /// 合成器子类型
    pub synth_subtype: i32,
    /// 打开
    pub open: Option<fn(dev: usize, mode: u32) -> i32>,
    /// 关闭
    pub close: Option<fn(dev: usize)>,
    /// ioctl
    pub ioctl: Option<fn(dev: usize, cmd: u32, arg: u32) -> i32>,
    /// 停止音符
    pub kill_note: Option<fn(dev: usize, voice: i32, velocity: i32) -> i32>,
    /// 开始音符
    pub start_note: Option<fn(dev: usize, voice: i32, note: i32, velocity: i32) -> i32>,
    /// 设置乐器
    pub set_instr: Option<fn(dev: usize, voice: i32, instr: i32) -> i32>,
    /// 复位
    pub reset: Option<fn(dev: usize)>,
    /// 硬件控制事件
    pub hw_control: Option<fn(dev: usize, event: *const u8)>,
    /// 加载音色
    pub load_patch: Option<fn(dev: usize, format: i32, addr: *const u8, offs: i32, count: i32, pmgr_flag: i32) -> i32>,
    /// 触后
    pub aftertouch: Option<fn(dev: usize, voice: i32, pressure: i32)>,
    /// 控制器
    pub controller: Option<fn(dev: usize, voice: i32, ctrl_num: i32, value: i32)>,
    /// 声像
    pub panning: Option<fn(dev: usize, voice: i32, value: i32)>,
    /// 音色管理器接口
    pub pmgr_interface: Option<fn(dev: usize, info: usize) -> i32>,
}

// ---- MIDI 操作虚表 ----
/// MIDI 信息。
/// 对应原版 `struct midi_info`。
#[derive(Default)]
pub struct MidiInfo {
    /// 设备名称
    pub name: &'static str,
}

/// MIDI 设备操作接口。
/// 对应原版 `struct midi_operations`。
pub struct MidiOperations {
    /// MIDI 信息
    pub info: MidiInfo,
    /// 打开
    pub open: Option<fn(dev: usize, mode: u32,
        input_intr: Option<fn(dev: usize, data: u8)>,
        output_intr: Option<fn(dev: usize)>) -> i32>,
    /// 关闭
    pub close: Option<fn(dev: usize)>,
    /// ioctl
    pub ioctl: Option<fn(dev: usize, cmd: u32, arg: u32) -> i32>,
    /// 发送一个字节
    pub putc: Option<fn(dev: usize, data: u8) -> i32>,
    /// 开始读取
    pub start_read: Option<fn(dev: usize) -> i32>,
    /// 结束读取
    pub end_read: Option<fn(dev: usize) -> i32>,
    /// 唤醒输出
    pub kick: Option<fn(dev: usize)>,
    /// 发送命令
    pub command: Option<fn(dev: usize, data: u8) -> i32>,
    /// 缓冲区状态
    pub buffer_status: Option<fn(dev: usize) -> i32>,
}

// ---- 全局设备注册表 ----
/// DSP 设备表。对应原版 `dsp_devs[MAX_DSP_DEV]`。
pub static mut DSP_DEVS: [Option<&'static AudioOperations>; MAX_DSP_DEV] = [None; MAX_DSP_DEV];
/// DSP 设备数量。对应原版 `num_dspdevs`。
pub static mut NUM_DSPDEVS: usize = 0;

/// 混音器设备表。对应原版 `mixer_devs[MAX_MIXER_DEV]`。
pub static mut MIXER_DEVS: [Option<&'static MixerOperations>; MAX_MIXER_DEV] = [None; MAX_MIXER_DEV];
/// 混音器数量。对应原版 `num_mixers`。
pub static mut NUM_MIXERS: usize = 0;

/// 合成器设备表。对应原版 `synth_devs[MAX_SYNTH_DEV]`。
pub static mut SYNTH_DEVS: [Option<&'static SynthOperations>; MAX_SYNTH_DEV] = [None; MAX_SYNTH_DEV];
/// 合成器数量。对应原版 `num_synths`。
pub static mut NUM_SYNTHS: usize = 0;

/// MIDI 设备表。对应原版 `midi_devs[MAX_MIDI_DEV]`。
pub static mut MIDI_DEVS: [Option<&'static MidiOperations>; MAX_MIDI_DEV] = [None; MAX_MIDI_DEV];
/// MIDI 设备数量。对应原版 `num_midis`。
pub static mut NUM_MIDIS: usize = 0;

// ---- DMA 配置表 ----
/// 缓冲区计数。对应原版 `sound_buffcounts[MAX_DSP_DEV]`。
pub static mut SOUND_BUFFCOUNTS: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
/// 缓冲区大小。对应原版 `sound_buffsizes[MAX_DSP_DEV]`。
pub static mut SOUND_BUFFSIZES: [usize; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
/// DSP DMA 通道号。对应原版 `sound_dsp_dmachan[MAX_DSP_DEV]`。
pub static mut SOUND_DSP_DMACHAN: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];
/// DMA 自动模式标志。对应原版 `sound_dma_automode[MAX_DSP_DEV]`。
pub static mut SOUND_DMA_AUTOMODE: [i32; MAX_DSP_DEV] = [0; MAX_DSP_DEV];

// ---- 声卡探测表 ----
/// 支持的声卡列表。对应原版 `supported_drivers[]`。
/// 探测顺序有语义依赖，不要随意调整。
pub static mut SUPPORTED_DRIVERS: [CardInfo; 8] = [
    CardInfo {
        card_type: SoundCardType::Mpu401,
        name: "Roland MPU-401",
        attach: None, // 待填充
        probe: None,
        config: AddressInfo { io_base: MPU_BASE, irq: MPU_IRQ, dma: 0 },
        enabled: false,
    },
    CardInfo {
        card_type: SoundCardType::Pas,
        name: "ProAudioSpectrum",
        attach: None,
        probe: None,
        config: AddressInfo { io_base: PAS_BASE, irq: PAS_IRQ, dma: PAS_DMA },
        enabled: false,
    },
    CardInfo {
        card_type: SoundCardType::Sb,
        name: "SoundBlaster",
        attach: None,
        probe: None,
        config: AddressInfo { io_base: SBC_BASE, irq: SBC_IRQ, dma: SBC_DMA },
        enabled: false,
    },
    CardInfo {
        card_type: SoundCardType::Sb16,
        name: "SoundBlaster16",
        attach: None,
        probe: None,
        config: AddressInfo { io_base: SBC_BASE, irq: SBC_IRQ, dma: SB16_DMA as u8 },
        enabled: false,
    },
    CardInfo {
        card_type: SoundCardType::Sb16Midi,
        name: "SB16 MPU-401",
        attach: None,
        probe: None,
        config: AddressInfo { io_base: SB16MIDI_BASE, irq: SBC_IRQ, dma: 0 },
        enabled: false,
    },
    CardInfo {
        card_type: SoundCardType::Gus,
        name: "Gravis Ultrasound",
        attach: None,
        probe: None,
        config: AddressInfo { io_base: GUS_BASE, irq: GUS_IRQ, dma: GUS_DMA },
        enabled: false,
    },
    CardInfo {
        card_type: SoundCardType::Adlib,
        name: "AdLib",
        attach: None,
        probe: None,
        config: AddressInfo { io_base: FM_MONO, irq: 0, dma: 0 },
        enabled: false,
    },
    // Sentinel: card_type == None 表示列表结束
    CardInfo {
        card_type: SoundCardType::None,
        name: "*?*",
        attach: None,
        probe: None,
        config: AddressInfo { io_base: 0, irq: 0, dma: 0 },
        enabled: false,
    },
];

/// 声卡驱动数量（含 sentinel）。对应原版 `num_sound_drivers`。
pub const NUM_SOUND_DRIVERS: usize = 8;

// ---- 辅助函数 ----

/// 注册一个音频设备。
///
/// # Safety
/// 启动期调用一次，不在中断上下文中。
pub unsafe fn register_audio_dev(ops: &'static AudioOperations) -> i32 {
    // SAFETY: 启动期单线程调用。
    let num = unsafe {
        let n = core::ptr::addr_of_mut!(NUM_DSPDEVS);
        let val = core::ptr::read_volatile(n);
        if val >= MAX_DSP_DEV {
            return -1;
        }
        let devs = core::ptr::addr_of_mut!(DSP_DEVS);
        (*devs)[val] = Some(ops);
        core::ptr::write_volatile(n, val + 1);
        val
    };
    num as i32
}

/// 注册一个混音器设备。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn register_mixer_dev(ops: &'static MixerOperations) -> i32 {
    unsafe {
        let n = core::ptr::addr_of_mut!(NUM_MIXERS);
        let val = core::ptr::read_volatile(n);
        if val >= MAX_MIXER_DEV {
            return -1;
        }
        let devs = core::ptr::addr_of_mut!(MIXER_DEVS);
        (*devs)[val] = Some(ops);
        core::ptr::write_volatile(n, val + 1);
        val as i32
    }
}

/// 注册一个合成器设备。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn register_synth_dev(ops: &'static SynthOperations) -> i32 {
    unsafe {
        let n = core::ptr::addr_of_mut!(NUM_SYNTHS);
        let val = core::ptr::read_volatile(n);
        if val >= MAX_SYNTH_DEV {
            return -1;
        }
        let devs = core::ptr::addr_of_mut!(SYNTH_DEVS);
        (*devs)[val] = Some(ops);
        core::ptr::write_volatile(n, val + 1);
        val as i32
    }
}

/// 注册一个 MIDI 设备。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn register_midi_dev(ops: &'static MidiOperations) -> i32 {
    unsafe {
        let n = core::ptr::addr_of_mut!(NUM_MIDIS);
        let val = core::ptr::read_volatile(n);
        if val >= MAX_MIDI_DEV {
            return -1;
        }
        let devs = core::ptr::addr_of_mut!(MIDI_DEVS);
        (*devs)[val] = Some(ops);
        core::ptr::write_volatile(n, val + 1);
        val as i32
    }
}

/// 获取已安装声卡的数量。
pub fn get_card_count() -> usize {
    // SAFETY: 只读
    unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(NUM_DSPDEVS))
            + core::ptr::read_volatile(core::ptr::addr_of!(NUM_MIXERS))
            + core::ptr::read_volatile(core::ptr::addr_of!(NUM_SYNTHS))
            + core::ptr::read_volatile(core::ptr::addr_of!(NUM_MIDIS))
    }
}

// ---- 声卡探测表初始化 ----
/// 初始化声卡探测表。遍历 `SUPPORTED_DRIVERS` 的每个条目，
/// 若 `enabled` 则调用 `probe`，探测成功则调用 `attach`。
///
/// 返回下一个可用内存地址（原版返回 `mem_start`）。
///
/// # Safety
/// 启动期间调用一次。会调用各声卡的 probe/attach 函数，
/// 这些函数会操作 I/O 端口、注册中断处理。
pub unsafe fn sndtable_init(mem_start: usize) -> usize {
    let mut mem = mem_start;

    for i in 0..(NUM_SOUND_DRIVERS - 1) {
        // SAFETY: 启动期单线程
        let driver = unsafe {
            &mut *core::ptr::addr_of_mut!(SUPPORTED_DRIVERS[i])
        };

        if !driver.enabled {
            continue;
        }

        let probe_fn = match driver.probe {
            Some(f) => f,
            None => {
                driver.enabled = false;
                continue;
            }
        };

        let attach_fn = match driver.attach {
            Some(f) => f,
            None => {
                driver.enabled = false;
                continue;
            }
        };

        // SAFETY: probe_fn 是声卡驱动提供的探测函数，契约由调用者保证
        if unsafe { probe_fn(&driver.config) } {
            // SAFETY: attach_fn 是声卡驱动提供的附加函数
            let card_type = driver.card_type;
            mem = unsafe { attach_fn(mem, &driver.config) };
            crate::kprintln!(
                "snd{} {} at 0x{:x} irq {} drq {}",
                card_type as u32,
                driver.name,
                driver.config.io_base,
                driver.config.irq,
                driver.config.dma,
            );
        } else {
            driver.enabled = false; // 未探测到则标记为未启用
        }
    }

    mem
}
