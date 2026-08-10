//! Yamaha YM3812 (OPL-2) 和 YMF262 (OPL-3) FM 合成器底层驱动。
//!
//! 对应 linux-1.0.9 的 `drivers/sound/opl3.c` + `opl3.h`。
//!
//! 提供：
//! - 芯片检测（`opl3_detect`）
//! - 寄存器读写（`opl3_command`）
//! - 音符的启动和停止（全局定时器/键盘）
//! - 常用于 AdLib 和 SoundBlaster 的 FM 合成

use super::config::*;

// ---- OPL-3 寄存器定义 ----
/// AM/VIB/EG/KSR/Multiple 寄存器基偏移
const AM_VIB: u8 = 0x20;
/// KSL / 总量电平 寄存器基偏移
const KSL_LEVEL: u8 = 0x40;
/// Attack / Decay 寄存器基偏移
const ATTACK_DECAY: u8 = 0x60;
/// Sustain / Release 寄存器基偏移
const SUSTAIN_RELEASE: u8 = 0x80;
/// F-Number 低位 寄存器基偏移
const FNUM_LOW: u8 = 0xA0;
/// Key On / Block (F-Number 高位) 寄存器基偏移
const KEYON_BLOCK: u8 = 0xB0;
/// Feedback / Connection 寄存器基偏移
const FEEDBACK_CONNECTION: u8 = 0xC0;
/// Percussion 寄存器 (左侧)
const PERCUSSION: u8 = 0xBD;
/// Wave Select 寄存器基偏移
const WAVE_SELECT: u8 = 0xE0;

/// Key On 位
const KEYON_BIT: u8 = 0x20;
/// 颤音深度
const TREMOLO_DEPTH: u8 = 0x80;
/// 振动深度
const VIBRATO_DEPTH: u8 = 0x40;
/// 鼓组启用
const PERCUSSION_ENABLE: u8 = 0x20;

/// OPL-3 启用寄存器偏移（右侧 0x05）
const OPL3_MODE_REGISTER: u16 = 0x05;
/// OPL-3 模式启用位
const OPL3_ENABLE: u8 = 0x01;

/// 连接选择寄存器（右侧 0x04）
const CONNECTION_SELECT_REGISTER: u16 = 0x04;

/// 合成器类型常量
const SYNTH_TYPE_FM: i32 = 1;
/// FM 类型：AdLib
const FM_TYPE_ADLIB: i32 = 0;

/// 最大 SBI 乐器数
const SBFM_MAXINSTR: usize = 256;
/// 最大音色数
const MAX_VOICE: usize = 18;

// ---- 物理音色定义表 ----
/// 物理音色信息。对应原版 `struct physical_voice_info`。
#[derive(Clone, Copy)]
struct PhysicalVoiceInfo {
    voice_num: u8,
    voice_mode: u8, // 0=unavailable, 2=2 OP, 4=4 OP
    side: u8,       // 0=left, 1=right
    op: [u8; 4],    // 算子偏移
}

/// 18 个物理音色定义。对应原版 `physical_voices[18]`。
static PHYSICAL_VOICES: [PhysicalVoiceInfo; 18] = [
    // 左侧
    PhysicalVoiceInfo { voice_num: 0, voice_mode: 2, side: 0, op: [0x00, 0x03, 0x08, 0x0b] },
    PhysicalVoiceInfo { voice_num: 1, voice_mode: 2, side: 0, op: [0x01, 0x04, 0x09, 0x0c] },
    PhysicalVoiceInfo { voice_num: 2, voice_mode: 2, side: 0, op: [0x02, 0x05, 0x0a, 0x0d] },
    PhysicalVoiceInfo { voice_num: 3, voice_mode: 2, side: 0, op: [0x08, 0x0b, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 4, voice_mode: 2, side: 0, op: [0x09, 0x0c, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 5, voice_mode: 2, side: 0, op: [0x0a, 0x0d, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 6, voice_mode: 2, side: 0, op: [0x10, 0x13, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 7, voice_mode: 2, side: 0, op: [0x11, 0x14, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 8, voice_mode: 2, side: 0, op: [0x12, 0x15, 0x00, 0x00] },
    // 右侧
    PhysicalVoiceInfo { voice_num: 0, voice_mode: 2, side: 1, op: [0x00, 0x03, 0x08, 0x0b] },
    PhysicalVoiceInfo { voice_num: 1, voice_mode: 2, side: 1, op: [0x01, 0x04, 0x09, 0x0c] },
    PhysicalVoiceInfo { voice_num: 2, voice_mode: 2, side: 1, op: [0x02, 0x05, 0x0a, 0x0d] },
    PhysicalVoiceInfo { voice_num: 3, voice_mode: 2, side: 1, op: [0x08, 0x0b, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 4, voice_mode: 2, side: 1, op: [0x09, 0x0c, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 5, voice_mode: 2, side: 1, op: [0x0a, 0x0d, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 6, voice_mode: 2, side: 1, op: [0x10, 0x13, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 7, voice_mode: 2, side: 1, op: [0x11, 0x14, 0x00, 0x00] },
    PhysicalVoiceInfo { voice_num: 8, voice_mode: 2, side: 1, op: [0x12, 0x15, 0x00, 0x00] },
];

// ---- 全局状态 ----
/// OPL-3 是否已启用
static mut OPL3_ENABLED: bool = false;
/// OPL-3 芯片是否已检测到
static mut OPL3_OK: bool = false;
/// FM 型号：0=无, 1=mono(OPL-2), 2=SB Pro 1, 3=SB Pro 2
static mut FM_MODEL: u32 = 0;
/// OPL-3 是否忙
static mut OPL3_BUSY: bool = false;
/// 已初始化标志
static mut ALREADY_INITIALIZED: bool = false;

/// 声部状态。对应原版 `struct voice_info`。
struct VoiceInfo {
    keyon_byte: u8,
    bender: i64,
    bender_range: i64,
    orig_freq: u64,
    current_freq: u64,
    mode: i32,
}

static mut VOICES: [VoiceInfo; MAX_VOICE] = [
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
    VoiceInfo { keyon_byte: 0, bender: 0, bender_range: 200, orig_freq: 0, current_freq: 0, mode: 0 },
];

// ---- OPL-3 命令写入 ----
/// 向 OPL-3 芯片发送一条命令（寄存器地址 + 值）。
/// 对应原版 `opl3_command()`。
///
/// OPL-3 芯片有两个寄存器对：
/// - 左侧：地址端口 = io_addr, 数据端口 = io_addr + 1
/// - 右侧 (opl3_mode)：地址端口 = io_addr + 2, 数据端口 = io_addr + 3
///
/// # Safety
/// `io_addr` 必须是有效的 OPL-3 I/O 基地址。
unsafe fn opl3_command(io_addr: u16, addr: u32, val: u32) {
    // SAFETY: 调用者保证 io_addr 有效
    // 原版先读 0x80 做 ISA 总线延迟，这里简化为直接写
    let left = io_addr;
    let right = io_addr + 2;

    // 判断地址属于左侧还是右侧
    // 右侧寄存器范围：0x100-0x1FF
    let (reg_port, data_port) = if addr >= 0x100 {
        let real_addr = (addr - 0x100) as u8;
        // SAFETY: I/O 端口是声卡探测到的
        unsafe {
            x86_64::instructions::port::PortWriteOnly::new(right).write(real_addr);
            x86_64::instructions::port::PortWriteOnly::new(right + 1).write(val as u8);
        }
        return;
    } else {
        (left, left + 1)
    };

    // SAFETY: I/O 端口操作
    unsafe {
        // 短暂延迟（原版: inb(io_addr), inb(io_addr)）
        for _ in 0..6 {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x80).write(0u8);
        }
        x86_64::instructions::port::PortWriteOnly::new(reg_port).write(addr as u8);
        for _ in 0..2 {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x80).write(0u8);
        }
        x86_64::instructions::port::PortWriteOnly::new(data_port).write(val as u8);
    }
}

// ---- 频率换算 ----
/// 频率 → F-Number + Block 换算。
/// 对应原版 `freq_to_fnum()`。
///
/// OPL-2/3 的音高公式：
/// freq = fnum * (50000 / 2^19) * 2^(block - 20)
fn freq_to_fnum(freq: u32) -> (u32, u32) {
    // block = log2(freq / fnum_scale)
    // fnum = freq * 2^(20 - block) * 2^19 / 50000
    let mut block: u32 = 0;
    let mut fnum: u32 = 0;

    if freq > 0 {
        // 找到合适的 block
        let mut f = freq;
        while f < 261 { // 约 50000/2^19 * 2^16
            f <<= 1;
            if block > 0 {
                block -= 1;
            } else {
                break;
            }
        }
        while f > 522 {
            f >>= 1;
            block += 1;
            if block >= 7 {
                block = 7;
                f = 522;
                break;
            }
        }
        fnum = freq.wrapping_mul((1u64 << 19) as u32) / 50000;
        if fnum > 0x3FF {
            fnum = 0x3FF; // 10-bit max
        }
    }

    (block & 0x07, fnum & 0x3FF)
}

// ---- 检测 ----
/// 检测 OPL-2/3 芯片是否存在。
/// 对应原版 `opl3_detect()`。
///
/// 通过写定时器寄存器并回读来检测。
///
/// # Safety
/// `io_addr` 必须是有效的 I/O 基地址。
pub unsafe fn opl3_detect(io_addr: u16) -> bool {
    let left = io_addr;

    // 写定时器 1 寄存器，延迟，再写定时器 2 寄存器
    // SAFETY: I/O 端口操作
    unsafe {
        x86_64::instructions::port::PortWriteOnly::new(left).write(0x02u8);  // TIMER1
        for _ in 0..12 {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x80).write(0u8);
        }
        x86_64::instructions::port::PortWriteOnly::new(left + 1).write(0xFFu8);
        x86_64::instructions::port::PortWriteOnly::new(left).write(0x04u8);  // TIMER_CONTROL
        for _ in 0..12 {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x80).write(0u8);
        }
        x86_64::instructions::port::PortWriteOnly::new(left + 1).write(0x60u8); // mask timer1+2, reset IRQ
    }

    for _ in 0..100 {
        core::hint::spin_loop();
    }

    // 读状态寄存器，检查 OPL-2 定时器中断标志
    // SAFETY: I/O 端口读
    let status = unsafe {
        let mut port = x86_64::instructions::port::PortReadOnly::<u8>::new(left);
        port.read()
    };

    // 复位定时器，检查状态是否正确复位
    unsafe {
        x86_64::instructions::port::PortWriteOnly::new(left + 1).write(0x80u8); // reset IRQ
        for _ in 0..12 {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x80).write(0u8);
        }
    }

    // 再次读状态——成功检测的标志是状态字节的 bit7 和 bit6 都清除
    let status2 = unsafe {
        let mut port = x86_64::instructions::port::PortReadOnly::<u8>::new(left);
        port.read()
    };

    (status & 0xe0) == 0x00 && (status2 & 0xe0) == 0x00
}

/// 启用 OPL-3 模式（如果芯片支持）。
/// 对应原版 `enable_opl3_mode()`。
///
/// # Safety
/// `io_addr` 必须是有效的 OPL-3 I/O 基地址。
pub unsafe fn enable_opl3_mode(io_addr: u16) {
    // SAFETY: io_addr 已由 opl3_detect 验证
    unsafe {
        opl3_command(io_addr, (OPL3_MODE_REGISTER | 0x100) as u32, OPL3_ENABLE as u32);
    }
}

// ---- 初始化 ----
/// 初始化 OPL-3 合成器。
/// 对应原版 `opl3_init()`。
///
/// # Safety
/// 调用前已确认 OPL-3 芯片存在（`opl3_detect` 返回 true）。
pub unsafe fn opl3_init(mem_start: usize) -> usize {
    // 先检测 OPL-3 能力
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(OPL3_ENABLED), true);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(OPL3_OK), true);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(FM_MODEL), 1); // mono OPL-2
    }

    // 尝试启用 OPL-3 模式
    // SAFETY: 已检测芯片存在
    unsafe {
        enable_opl3_mode(FM_MONO);
    }

    // 复位所有音色
    for i in 0..MAX_VOICE {
        // SAFETY: 在已持有锁时调用
        unsafe {
            // 停止音符
            let pv = &PHYSICAL_VOICES[i];
            let io_addr = if pv.side == 0 { FM_MONO } else { FM_MONO + 2 };
            let keyon_reg = (KEYON_BLOCK as u32) + pv.voice_num as u32;
            opl3_command(io_addr, keyon_reg, pv.op[0] as u32 & !(KEYON_BIT as u32));
        }
    }

    // 设置基本音色参数：所有算子默认设置
    for i in 0..MAX_VOICE {
        let pv = &PHYSICAL_VOICES[i];
        let io_addr = if pv.side == 0 { FM_MONO } else { FM_MONO + 2 };

        // 为每个算子设置默认 Attack/Decay/Sustain/Release/KSL 等参数
        for op_idx in 0..2 {
            let op = pv.op[op_idx] as u32;
            // SAFETY: 已持有 OPL-3
            unsafe {
                opl3_command(io_addr, (AM_VIB as u32) + op, 0x21);   // AM=0, VIB=0, S=1, KSR=0, MULT=1
                opl3_command(io_addr, (KSL_LEVEL as u32) + op, 0x3F); // KSL=0, TotalLevel=max(quiet)
                opl3_command(io_addr, (ATTACK_DECAY as u32) + op, 0x99); // Attack=9, Decay=9
                opl3_command(io_addr, (SUSTAIN_RELEASE as u32) + op, 0x0F); // Sustain=0, Release=15
            }
        }
        // Feedback/Connection: feedback=6, connection=0(FM synthesis)
        unsafe {
            let conn_reg = (FEEDBACK_CONNECTION as u32) + pv.voice_num as u32;
            opl3_command(io_addr, conn_reg, 0x30); // feedback=6, AM=0, stereo=both
        }
    }

    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(ALREADY_INITIALIZED), true);
    }

    crate::kprintln!("opl3: OPL-3/AdLib FM synthesizer initialized");
    mem_start
}

// ---- 音符操作 ----
/// 停止指定声部的音符。
///
/// # Safety
/// `dev` 必须是有效的 OPL-3 设备索引。
pub unsafe fn opl3_kill_note(voice: usize) {
    if voice >= MAX_VOICE {
        return;
    }

    let pv = &PHYSICAL_VOICES[voice];
    let io_addr = if pv.side == 0 { FM_MONO } else { FM_MONO + 2 };
    let keyon_reg = (KEYON_BLOCK as u32) + pv.voice_num as u32;

    // SAFETY: 调用者保证 voice 有效
    unsafe {
        // 写入不带 KEYON_BIT 的 keyon_byte
        let voice_data = core::ptr::read_volatile(core::ptr::addr_of!(VOICES[voice]));
        let keyoff = voice_data.keyon_byte & !KEYON_BIT;
        opl3_command(io_addr, keyon_reg, keyoff as u32);
    }
}

/// 通过整数运算计算 MIDI 音符频率（避免大表和 powf）。
/// A4 (MIDI note 69) = 440 Hz。
/// 使用移位累加近似 2^(n/12)。
fn midi_note_to_freq(note: i32) -> u32 {
    if note < 0 || note > 127 {
        return 0;
    }
    // 440 * 2^((note-69)/12)
    // 用固定点近似：2^(semitone/12) ≈ (1024 + semitone*59) / 1024
    let semitone = note - 69;
    let octave_shift: i32 = semitone.div_euclid(12);
    let semitone_in_octave: i32 = semitone.rem_euclid(12);

    // 预计算的 12 个半音乘数 × 1024
    const SEMITONE_MUL: [u32; 12] = [1024, 1085, 1149, 1218, 1290, 1367, 1448, 1534, 1625, 1722, 1824, 1933];

    let base = SEMITONE_MUL[semitone_in_octave as usize] as u64;
    let freq = (440u64 * base + 512) / 1024;
    let result = if octave_shift >= 0 {
        (freq << octave_shift as u32) as u32
    } else {
        (freq >> (-octave_shift) as u32) as u32
    };

    if result == 0 { 1 } else { result }
}

/// 开始演奏指定声部的音符。
///
/// # Safety
/// `voice` 必须是有效的声部索引。
pub unsafe fn opl3_start_note(voice: usize, note: i32, _velocity: i32) {
    if voice >= MAX_VOICE {
        return;
    }

    // MIDI note → frequency（整数近似，避免 f64::powf 和查找表）
    let freq = midi_note_to_freq(note);
    if freq == 0 {
        return;
    }

    let (block, fnum) = freq_to_fnum(freq);
    let pv = &PHYSICAL_VOICES[voice];
    let io_addr = if pv.side == 0 { FM_MONO } else { FM_MONO + 2 };

    // SAFETY: pv 在静态表中，io_addr 由检测确定
    unsafe {
        // 写 F-Number 低 8 位
        let fnum_low_reg = (FNUM_LOW as u32) + pv.voice_num as u32;
        opl3_command(io_addr, fnum_low_reg, fnum & 0xFF);

        // 写 Key On / Block / F-Number 高 2 位
        let keyon_reg = (KEYON_BLOCK as u32) + pv.voice_num as u32;
        let keyon_val = KEYON_BIT as u32 | (block << 2) | ((fnum >> 8) & 0x03);

        // 保存 keyon_byte
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(VOICES[voice].keyon_byte),
            keyon_val as u8,
        );

        opl3_command(io_addr, keyon_reg, keyon_val);
    }
}

// ---- 公共接口 ----

/// 向 OPL-3 芯片写入通用命令。
///
/// # Safety
/// 同 `opl3_command`。
pub unsafe fn opl3_write(io_addr: u16, reg: u16, val: u8) {
    unsafe {
        opl3_command(io_addr, reg as u32, val as u32);
    }
}

/// 获取 OPL-3 是否已初始化。
pub fn opl3_is_initialized() -> bool {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(ALREADY_INITIALIZED)) }
}

/// 获取 OPL-3 声部数。
pub fn opl3_nr_voices() -> usize {
    MAX_VOICE
}
