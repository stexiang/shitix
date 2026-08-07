//! AdLib 声卡（YM3812 OPL-2）驱动。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/adlib_card.c`。
//!
//! AdLib 是最简单的声卡之一——只有一个 OPL-2 FM 合成器，没有
//! DMA 音频或 MIDI。只需检测 OPL-2 芯片并初始化合成器即可。

use super::config::*;
use super::dev_table;
use super::opl3;

/// 探测 AdLib 声卡。对应原版 `probe_adlib()`。
///
/// 只需检测 OPL-2 芯片在 0x388 端口是否存在。
pub fn probe_adlib(_hw_config: &AddressInfo) -> bool {
    // SAFETY: 只检测 I/O 端口，不修改任何状态
    unsafe { opl3::opl3_detect(FM_MONO) }
}

/// 附加 AdLib 声卡。对应原版 `attach_adlib_card()`。
pub fn attach_adlib(mem_start: usize, _hw_config: &AddressInfo) -> usize {
    if probe_adlib(&AddressInfo::default()) {
        // SAFETY: 已确认 OPL-2 芯片存在
        unsafe { opl3::opl3_init(mem_start) }
    } else {
        mem_start
    }
}

/// 初始化 AdLib 卡在全局驱动表中的条目。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn init_adlib_driver() {
    // 在 SUPPORTED_DRIVERS 中找到 AdLib 条目并配置
    let drivers = unsafe { &mut *core::ptr::addr_of_mut!(dev_table::SUPPORTED_DRIVERS) };
    for i in 0..dev_table::NUM_SOUND_DRIVERS.saturating_sub(1) {
        if drivers[i].card_type as u32 == SoundCardType::Adlib as u32 {
            drivers[i].probe = Some(probe_adlib);
            drivers[i].attach = Some(attach_adlib);
            // 默认启用 AdLib——只检测端口，不抢占 DMA/IRQ，安全
            drivers[i].enabled = true;
            break;
        }
    }
}
