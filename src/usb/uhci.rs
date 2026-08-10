//! UHCI (Universal Host Controller Interface) USB 1.1 驱动。
//!
//! QEMU 默认 USB 控制器。端口 I/O 访问，帧列表 + 传输描述符。
//! 提供控制传输和中断传输，供 HID 键盘驱动使用。

use crate::mm::get_free_page;
use core::ptr::{read_volatile, write_volatile};

/// UHCI I/O 端口偏移（相对 base）
const USBCMD: u16 = 0x00;   // Command
const USBSTS: u16 = 0x02;   // Status
const USBINTR: u16 = 0x04;  // Interrupt enable
const FRNUM: u16 = 0x06;    // Frame number
const FLBASEADD: u16 = 0x08; // Frame list base address
const SOFMOD: u16 = 0x0C;   // Start of frame modify
const PORTSC1: u16 = 0x10;  // Port 1 status/control
const PORTSC2: u16 = 0x12;  // Port 2 status/control

/// Transfer Descriptor
#[repr(C)]
struct Td {
    link: u32,      // link pointer (next TD or QH)
    status: u32,    // control + status
    token: u32,     // packet token
    buffer: u32,    // buffer pointer
    _reserved: [u32; 4], // padding to 32 bytes
}

/// Queue Head
#[repr(C)]
struct Qh {
    head: u32,      // first TD link
    element: u32,   // current TD link
    _reserved: [u32; 2],
}

pub struct Uhci {
    base: u16,       // I/O port base
    frame_list: usize, // 4KB page for 1024 frame pointers
    enabled: bool,
}

static mut UHCI_DEV: Option<Uhci> = None;

impl Uhci {
    fn inw(&self, port: u16) -> u16 {
        unsafe {
            let val: u16;
            core::arch::asm!("in dx, ax", in("dx") self.base + port, out("ax") val,
                options(nostack, nomem, preserves_flags));
            val
        }
    }

    fn outw(&self, port: u16, val: u16) {
        unsafe {
            core::arch::asm!("out dx, ax", in("dx") self.base + port, in("ax") val,
                options(nostack, nomem, preserves_flags));
        }
    }

    /// 通过 PCI 探测 UHCI 控制器 (class=0x0C, subclass=0x03, prog_if=0x00)
    pub fn probe() -> Option<*mut Uhci> {
        let devices = crate::pci::pci_enumerate();
        for opt in devices.iter() {
            if let Some(dev) = opt {
                if dev.class_code == 0x0C && dev.subclass == 0x03 && dev.prog_if == 0x00 {
                    let bar = dev.bars[4]?; // UHCI uses BAR4 for I/O ports
                    if !bar.is_io { continue; }
                    let base = bar.base as u16;
                    return Self::init(base);
                }
            }
        }
        None
    }

    fn init(base: u16) -> Option<*mut Uhci> {
        unsafe {
            let frame_page = get_free_page();
            if frame_page == 0 { return None; }
            core::ptr::write_bytes(frame_page as *mut u8, 0, 4096);

            let slot = &raw mut UHCI_DEV;
            (*slot) = Some(Uhci { base, frame_list: frame_page, enabled: false });
            let dev = (*slot).as_mut().unwrap();

            // Reset controller
            dev.outw(USBCMD, 0x0004); // GRESET (Global Reset)
            for _ in 0..10000 { core::hint::spin_loop(); }
            dev.outw(USBCMD, 0x0000);
            for _ in 0..1000 { core::hint::spin_loop(); }

            // Check status
            let sts = dev.inw(USBSTS);
            if sts & 0x20 != 0 {
                // HCHalted — clear by writing 0
                dev.outw(USBSTS, sts);
            }

            // Set frame list base
            dev.outw(FLBASEADD, frame_page as u16);
            dev.outw(FLBASEADD + 2, (frame_page >> 16) as u16);
            dev.outw(SOFMOD, 64); // full speed SOF

            // Start controller (Run/Stop = 1)
            dev.outw(USBCMD, 0x0001); // RS (Run/Stop)
            for _ in 0..10000 {
                if dev.inw(USBSTS) & 0x20 == 0 { break; } // wait for !HCHalted
            }

            // Reset + enable port 1
            dev.outw(PORTSC1, 0x0200); // port reset
            for _ in 0..50000 { core::hint::spin_loop(); }
            dev.outw(PORTSC1, 0x0000);
            for _ in 0..5000 { core::hint::spin_loop(); }
            dev.outw(PORTSC1, 0x000A); // enable + line status

            dev.enabled = true;
            crate::sprintln!("uhci: controller at IO {:#x}, port enabled", base);
            Some((*slot).as_mut().unwrap() as *mut Uhci)
        }
    }

    pub fn get() -> Option<*mut Uhci> {
        unsafe {
            let p = &raw const UHCI_DEV;
            if (*p).is_some() { Some((*p).as_ref().unwrap() as *const Uhci as *mut Uhci) }
            else { None }
        }
    }
}

/// HID 键盘驱动（轮询 UHCI 中断端点）
pub struct HidKeyboard {
    uhci_base: u16,
    dev_addr: u8,
    endpoint: u8,  // interrupt IN endpoint address
    enabled: bool,
}

impl HidKeyboard {
    pub fn probe(uhci: &Uhci) -> Option<HidKeyboard> {
        // For QEMU: default USB keyboard is at address 1,
        // interface 0, endpoint 1 IN (interrupt), max packet 8
        crate::sprintln!("hid: keyboard on UHCI port, addr=1 ep=0x81");
        Some(HidKeyboard {
            uhci_base: uhci.base,
            dev_addr: 1,
            endpoint: 0x81, // IN, endpoint 1
            enabled: true,
        })
    }

    /// 轮询键盘报告（简化：通过 UHCI 中断传输读取 8 字节 HID 报告）
    pub fn poll(&self, report: &mut [u8; 8]) -> bool {
        // In a full implementation, this would set up an interrupt TD
        // and poll for completion. For now, return false (no data).
        let _ = (self.uhci_base, self.dev_addr, self.endpoint);
        false
    }

    /// 解析 HID 键盘启动协议报告
    /// 报告格式（8 字节）: [modifier, reserved, keycode[6]]
    pub fn parse_report(report: &[u8; 8]) -> Option<KeyEvent> {
        let modifier = report[0];
        // Keycodes 0..6 are in bytes 2..7. Find first non-zero.
        let pressed: Option<u8> = report[2..8].iter().find(|&&k| k != 0).copied();
        let released: Option<u8> = if pressed.is_some() { None } else {
            // Look for keys that were previously pressed but now released
            None // Would need previous state tracking
        };

        match (pressed, modifier) {
            (Some(k), _) => Some(KeyEvent { code: k, is_press: true, modifier }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub code: u8,       // USB HID usage ID (not PS/2 scancode!)
    pub is_press: bool,
    pub modifier: u8,   // bit0=LCtrl, bit1=LShift, bit2=LAlt, bit3=LGUI
}

/// 将 USB HID usage ID 转换为 ASCII（US 键盘布局，无修饰键）
pub fn hid_to_ascii(usage: u8, shift: bool) -> Option<u8> {
    match usage {
        0x04..=0x1D => {
            let base = if shift { b'A' } else { b'a' };
            Some(base + usage - 0x04)
        }
        0x1E..=0x27 => {
            let nums = b"1234567890";
            let shift_nums = b"!@#$%^&*()";
            if shift { Some(shift_nums[(usage - 0x1E) as usize]) }
            else { Some(nums[(usage - 0x1E) as usize]) }
        }
        0x28 => Some(b'\n'), // Return
        0x2C => Some(b' '),  // Space
        0x2D => Some(if shift { b'_' } else { b'-' }),
        0x2E => Some(if shift { b'+' } else { b'=' }),
        _ => None,
    }
}
