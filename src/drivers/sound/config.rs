//! 声卡子系统配置与类型定义。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/sound_config.h`。

/// 支持的声卡类型枚举。
/// 对应原版 `supported_drivers` 表中各 card_type。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SoundCardType {
    /// Sentinel：列表结束标记
    None = 0,
    /// Roland MPU-401 MIDI 接口
    Mpu401 = 1,
    /// ProAudioSpectrum 16
    Pas = 2,
    /// SoundBlaster / SB Pro / SB16
    Sb = 3,
    /// SoundBlaster 16（独立 16 位 DSP）
    Sb16 = 4,
    /// SoundBlaster 16 上的 MPU-401 MIDI
    Sb16Midi = 5,
    /// Gravis Ultrasound
    Gus = 6,
    /// AdLib (YM3812/OPL-2)
    Adlib = 7,
}

/// 声卡 I/O 地址 / IRQ / DMA 配置。
/// 对应原版 `struct address_info`。
#[derive(Debug, Clone, Copy, Default)]
pub struct AddressInfo {
    /// I/O 基地址
    pub io_base: u16,
    /// 中断号
    pub irq: u8,
    /// DMA 通道号
    pub dma: u8,
}

/// 声卡探测/附加信息。
/// 对应原版 `struct card_info`。
pub struct CardInfo {
    /// 声卡类型
    pub card_type: SoundCardType,
    /// 显示名
    pub name: &'static str,
    /// 附加回调：分配资源、注册设备
    pub attach: Option<fn(mem_start: usize, hw_config: &AddressInfo) -> usize>,
    /// 探测回调：检测硬件是否存在
    pub probe: Option<fn(hw_config: &AddressInfo) -> bool>,
    /// 硬件配置
    pub config: AddressInfo,
    /// 是否启用
    pub enabled: bool,
}

/// 文件打开模式。对应原版 `OPEN_READ`/`OPEN_WRITE`/`OPEN_READWRITE`。
pub mod open_mode {
    pub const OPEN_READ: u32 = 1;
    pub const OPEN_WRITE: u32 = 2;
    pub const OPEN_READWRITE: u32 = 3;
}

/// 文件信息。对应原版 `struct fileinfo`。
#[derive(Debug, Clone, Copy, Default)]
pub struct FileInfo {
    /// 打开模式（见 `open_mode`）
    pub mode: u32,
}

// ---- 设备号定义 ----
/// 支持的最大设备数。对应原版 `SND_NDEVS 50`。
pub const SND_NDEVS: usize = 50;

/// 次设备号：控制端口 /dev/mixer
pub const SND_DEV_CTL: u32 = 0;
/// 次设备号：音序器 /dev/sequencer
pub const SND_DEV_SEQ: u32 = 1;
/// 次设备号：MIDI 输入 /dev/midin
pub const SND_DEV_MIDIN: u32 = 2;
/// 次设备号：数字音频 /dev/dsp
pub const SND_DEV_DSP: u32 = 3;
/// 次设备号：音频 /dev/audio（SPARC 兼容）
pub const SND_DEV_AUDIO: u32 = 4;
/// 次设备号：16 位 DSP /dev/dsp16
pub const SND_DEV_DSP16: u32 = 5;
/// 次设备号：状态 /dev/sndstatus
pub const SND_DEV_STATUS: u32 = 6;

// ---- DMA 参数 ----
/// 最大 DSP 设备数。对应原版 `MAX_DSP_DEV 4`。
pub const MAX_DSP_DEV: usize = 4;
/// 最大混音器设备数。对应原版 `MAX_MIXER_DEV 2`。
pub const MAX_MIXER_DEV: usize = 2;
/// 最大合成器设备数。对应原版 `MAX_SYNTH_DEV 3`。
pub const MAX_SYNTH_DEV: usize = 3;
/// 最大 MIDI 设备数。对应原版 `MAX_MIDI_DEV 4`。
pub const MAX_MIDI_DEV: usize = 4;

/// DMA 缓冲区计数。对应原版 `DSP_BUFFCOUNT`。
pub const DSP_BUFFCOUNT: usize = 2;
/// 默认 PCM 采样率。对应原版 `DSP_DEFAULT_SPEED 8000`。
pub const DSP_DEFAULT_SPEED: u32 = 8000;
/// 实时因子上限。对应原版 `MAX_REALTIME_FACTOR 4`。
pub const MAX_REALTIME_FACTOR: usize = 4;
/// 逻辑子缓冲区最大数量。
pub const MAX_SUB_BUFFERS: usize = 32 * MAX_REALTIME_FACTOR;

/// 音序器最大队列。对应原版 `SEQ_MAX_QUEUE 1024`。
pub const SEQ_MAX_QUEUE: usize = 1024;

// ---- DMA 模式 ----
/// DMA 模式：无
pub const DMODE_NONE: u32 = 0;
/// DMA 模式：输出（播放）
pub const DMODE_OUTPUT: u32 = 1;
/// DMA 模式：输入（录制）
pub const DMODE_INPUT: u32 = 2;

/// DMA 自动初始化模式标志。对应原版 `DMA_AUTOINIT 0x10`。
pub const DMA_AUTOINIT: u32 = 0x10;

// ---- 声卡主设备号 ----
/// 声卡主设备号。对应原版 `include/linux/major.h` 的 `SOUND_MAJOR`。
pub const SOUND_MAJOR: u32 = 14;

// ---- I/O 地址默认值 ----
/// SoundBlaster 默认基地址
pub const SBC_BASE: u16 = 0x220;
/// SoundBlaster 默认 IRQ
pub const SBC_IRQ: u8 = 7;
/// SoundBlaster 默认 8-bit DMA
pub const SBC_DMA: u8 = 1;
/// SB16 默认 16-bit DMA
pub const SB16_DMA: u8 = 6;
/// SB16 MIDI 默认基地址
pub const SB16MIDI_BASE: u16 = 0x300;
/// ProAudioSpectrum 默认基地址
pub const PAS_BASE: u16 = 0x388;
/// PAS 默认 IRQ
pub const PAS_IRQ: u8 = 5;
/// PAS 默认 DMA
pub const PAS_DMA: u8 = 3;
/// Gravis Ultrasound 默认基地址
pub const GUS_BASE: u16 = 0x220;
/// GUS 默认 IRQ
pub const GUS_IRQ: u8 = 15;
/// GUS 默认 DMA
pub const GUS_DMA: u8 = 6;
/// MPU-401 默认基地址
pub const MPU_BASE: u16 = 0x330;
/// MPU-401 默认 IRQ
pub const MPU_IRQ: u8 = 5;
/// AdLib / OPL-2 默认 I/O 地址
pub const FM_MONO: u16 = 0x388;

// ---- 唤醒原因掩码 ----
pub const WK_NONE: u32 = 0x00;
pub const WK_WAKEUP: u32 = 0x01;
pub const WK_TIMEOUT: u32 = 0x02;
pub const WK_SIGNAL: u32 = 0x04;
pub const WK_SLEEP: u32 = 0x08;

// ---- SoundBlaster 寄存器偏移 (相对于 sbc_base) ----
/// DSP 复位
pub const DSP_RESET: u16 = 0x06;
/// DSP 读取
pub const DSP_READ: u16 = 0x0A;
/// DSP 写入/命令
pub const DSP_WRITE: u16 = 0x0C;
/// DSP 状态
pub const DSP_STATUS: u16 = 0x0E;
/// DSP 中断应答（8bit）
pub const DSP_DATA_AVAIL: u16 = 0x0E;
/// DSP 中断应答（16bit）
pub const DSP_DATA_AVAIL16: u16 = 0x0F;

// ---- SoundBlaster 混音器寄存器和芯片号 ----
/// 混音器基地址（相对于 sbc_base）
pub const MIXER_ADDR: u16 = 0x04;
/// 混音器数据
pub const MIXER_DATA: u16 = 0x05;

/// 芯片类型：SoundBlaster 1.0
pub const SB_10: u32 = 1;
/// 芯片类型：SoundBlaster 1.5
pub const SB_15: u32 = 2;
/// 芯片类型：SoundBlaster 2.0
pub const SB_20: u32 = 3;
/// 芯片类型：SoundBlaster Pro
pub const SB_PRO: u32 = 4;
/// 芯片类型：SoundBlaster 16
pub const SB_16: u32 = 6;

/// DSP 主音量
pub const SB_MIXER_MASTER_VOL: u8 = 0x22;
/// DSP 语音音量
pub const SB_MIXER_VOICE_VOL: u8 = 0x04;
/// DSP MIDI 音量
pub const SB_MIXER_MIDI_VOL: u8 = 0x26;
/// DSP CD 音量
pub const SB_MIXER_CD_VOL: u8 = 0x28;
/// DSP Line-in 音量
pub const SB_MIXER_LINE_VOL: u8 = 0x2E;
/// DSP 麦克风音量
pub const SB_MIXER_MIC_VOL: u8 = 0x0A;
/// 输入源选择
pub const SB_MIXER_INPUT_SRC: u8 = 0x0C;
/// 输出滤波
pub const SB_MIXER_OUTPUT_FILTER: u8 = 0x0E;

// ---- OPL-3 寄存器地址 (相对于 FM_MONO=0x388) ----
/// OPL-3 地址寄存器（端口 0）
pub const OPL3_LEFT: u16 = 0x00;
/// OPL-3 数据寄存器（端口 1）
pub const OPL3_RIGHT: u16 = 0x01;
/// OPL-3 地址寄存器（端口 2，用于 OPL-3 模式）
pub const OPL3_BOTH: u16 = 0x02;
/// OPL-3 数据寄存器（端口 3）
pub const OPL3_BOTH_DATA: u16 = 0x03;
/// OPL-3 定时器 1 计数器
pub const OPL3_TIMER1: u8 = 0x02;
/// OPL-3 定时器 2 计数器
pub const OPL3_TIMER2: u8 = 0x03;
/// OPL-3 定时器控制
pub const OPL3_TIMER_CONTROL: u8 = 0x04;
/// OPL-3 新设备使能（4-operator 模式）
pub const OPL3_MODE_ENABLE: u16 = 0x105;
/// OPL-3 合成模式
pub const OPL3_COMPOSITE_SINE_MODE: u16 = 0x108;

// ---- GUS 相关常量 ----
/// GUS 最大合成音色数
pub const GUS_NUM_VOICES: usize = 32;
/// GUS MIDI 缓冲区大小
pub const GUS_MIDI_BUF_SIZE: usize = 256;
