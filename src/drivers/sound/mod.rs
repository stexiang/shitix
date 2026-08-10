//! 声卡子系统。对应 linux-1.0.9 的 `drivers/sound/`。
//!
//! # 移植范围
//!
//! | 本模块 | 原版 | 说明 |
//! |---|---|---|
//! | [`config`] | `sound_config.h` | 配置常量、类型定义（声卡类型、DMA 参数、I/O 地址等）|
//! | [`dev_table`] | `dev_table.h` + `dev_table.c` | 设备虚表（audio_ops / mixer_ops / synth_ops / midi_ops）与全局注册表 |
//! | [`soundcard`] | `soundcard.c` | 声卡子系统入口：注册字符设备、探测/附加、中断/定时器辅助 |
//! | [`sound_switch`] | `sound_switch.c` | VFS 分派层：按次设备号路由 read/write/open/release/ioctl |
//! | [`dmabuf`] | `dmabuf.c` | DMA 缓冲区管理器：物理缓冲分配、逻辑子缓冲拆分、环形队列、中断处理 |
//! | [`audio`] | `audio.c` | /dev/dsp、/dev/audio 设备文件读写与 μ-law 转换 |
//! | [`opl3`] | `opl3.c` + `opl3.h` | Yamaha YM3812 (OPL-2) / YMF262 (OPL-3) FM 合成器底层驱动 |
//! | [`sb`] | `sb_card.c` + `sb_dsp.c` + `sb_mixer.c` + `sb_midi.c` + `sb.h` + `sb_mixer.h` | SoundBlaster 全系列驱动 |
//! | [`adlib`] | `adlib_card.c` | AdLib 声卡（最简：只有 OPL-2 FM 合成器）|
//!
//! 未移植：Gravis Ultrasound (`gus_*.c`)、ProAudioSpectrum (`pas2_*.c`)、
//! MPU-401 MIDI (`mpu401.c`)、音序器 (`sequencer.c` + `midibuf.c` +
//! `patmgr.c`)、SoundBlaster 16 DSP (`sb16_dsp.c` + `sb16_midi.c`)、
//! 配置程序 (`configure.c`，用户态工具)。

pub mod config;
pub mod dev_table;
pub mod soundcard;
pub mod sound_switch;
pub mod dmabuf;
pub mod audio;
pub mod opl3;
pub mod sb;
pub mod adlib;

/// 初始化声卡子系统。
///
/// 在 `dev_table::sndtable_init` 之前，先启用 AdLib 和 SoundBlaster
/// 驱动的探测表条目，然后调用 `soundcard::soundcard_init()`。
///
/// # Safety
/// 启动期调用一次。会直接操作 I/O 端口并注册中断处理。
pub unsafe fn init() {
    // 启用各声卡驱动的探测表条目
    // SAFETY: 启动期单线程
    unsafe {
        adlib::init_adlib_driver();
        sb::init_sb_driver();
    }

    // 初始化 DMAbuf（需在各声卡注册之后）
    dmabuf::dmabuf_init();

    // 主声卡初始化（探测 + 附加 + 注册字符设备）
    // SAFETY: 启动期调用
    unsafe {
        soundcard::soundcard_init();
    }
}
