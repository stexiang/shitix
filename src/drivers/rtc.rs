//! CMOS RTC（实时时钟）。对应 linux-1.0.9 的 `kernel/time.c:time_init()`
//! 里从 CMOS 读年月日时分秒再 `mktime()` 的那段。
//!
//! QEMU 的 RTC 默认宿主机 UTC 时间。寄存器通过索引口 0x70 / 数据口 0x71
//! 访问，时间字段可能是 BCD 也可能是二进制，由寄存器 B 的 bit2(DM) 决定。
//! 读之前等寄存器 A 的 bit7(UIP) 清 0，避免读到更新到一半的值
//! （原版 `CMOS_READ` 宏同款自旋）。

/// CMOS 索引口 / 数据口
const CMOS_ADDR: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

/// # Safety
/// 内核态 I/O 端口访问。
unsafe fn outb(port: u16, val: u8) {
    // SAFETY: 调用者保证 CPL=0。
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nomem, nostack, preserves_flags)
        )
    }
}

/// # Safety
/// 内核态 I/O 端口访问。
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    // SAFETY: 调用者保证 CPL=0。
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") val,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        )
    }
    val
}

/// 读一个 CMOS 寄存器。对应原版 `CMOS_READ(addr)`。
///
/// # Safety
/// 内核态，启动期调用。
unsafe fn cmos_read(addr: u8) -> u8 {
    // SAFETY: 契约转交。
    unsafe {
        // 等 UIP（Update In Progress）清 0，与原版一致
        loop {
            outb(CMOS_ADDR, 0x0A);
            if inb(CMOS_DATA) & 0x80 == 0 {
                break;
            }
        }
        outb(CMOS_ADDR, addr);
        inb(CMOS_DATA)
    }
}

/// BCD → 二进制。寄存器 B 的 bit2(DM)=1 表示已是二进制。
fn bcd_to_bin(v: u8, is_bin: bool) -> u32 {
    if is_bin {
        v as u32
    } else {
        ((v >> 4) as u32) * 10 + (v & 0x0F) as u32
    }
}

/// 年月日 → Unix 秒。对应原版 `kernel/mktime.c:kernel_mktime()`。
fn mktime(year: u32, mon: u32, day: u32, hour: u32, min: u32, sec: u32) -> u32 {
    // 原版技巧：把 1/2 月当作上一年的 13/14 月，闰年规则只在年内生效
    let (y, m) = if mon <= 2 { (year - 1, mon + 10) } else { (year, mon - 2) };
    let (y, m, day) = (y as i32, m as i32, day as i32);
    // 从 1970-03-01 起算的天数
    let days = (y / 4 - y / 100 + y / 400 + 367 * m / 12 + day + y * 365 - 719499_i32) as u32;
    days * 86400 + hour * 3600 + min * 60 + sec
}

/// 读出 RTC 当前时间（Unix 秒）。读取失败/字段全 0 返回 None。
///
/// # Safety
/// 启动期调用一次。
pub unsafe fn read_time() -> Option<u32> {
    // SAFETY: 契约转交。
    unsafe {
        let reg_b = cmos_read(0x0B);
        let is_bin = reg_b & 0x04 != 0;
        let is_24h = reg_b & 0x02 != 0;

        let sec = bcd_to_bin(cmos_read(0x00), is_bin);
        let min = bcd_to_bin(cmos_read(0x02), is_bin);
        let mut hour = bcd_to_bin(cmos_read(0x04) & 0x7F, is_bin);
        let pm = cmos_read(0x04) & 0x80 != 0;
        if !is_24h && pm {
            hour += 12;
        }
        let day = bcd_to_bin(cmos_read(0x07), is_bin);
        let mon = bcd_to_bin(cmos_read(0x08), is_bin);
        let mut year = bcd_to_bin(cmos_read(0x09), is_bin);
        // CMOS 世纪寄存器（0x32）很多机器没有；两位年份按 2000 起算
        // （原版 1.0.9 也这么干，它的注释是「1995 年以后写的代码总该
        // 活着见到 2000 年」）。
        if year < 70 {
            year += 2000;
        } else if year < 100 {
            year += 1900;
        }

        if mon == 0 || mon > 12 || day == 0 || day > 31 || hour > 24 || min > 59 || sec > 59 {
            return None;
        }
        Some(mktime(year, mon, day, hour, min, sec))
    }
}

/// 初始化：读 RTC，把开机时刻写进 `sched::set_startup_time()`，
/// 让 `current_time()`（→ inode 时间戳、`sys_time`）返回真实墙钟。
///
/// # Safety
/// 启动期调用一次，需在 `sched::init()` 之后。
pub unsafe fn init() {
    // SAFETY: 契约转交。
    unsafe {
        match read_time() {
            Some(t) => {
                crate::sched::set_startup_time(t);
                crate::pr_info!("rtc: startup_time = {} (unix)", t);
            }
            None => {
                crate::pr_warn!("rtc: CMOS read failed, startup_time stays 0");
            }
        }
    }
}
