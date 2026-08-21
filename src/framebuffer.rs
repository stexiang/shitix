//! VESA 线性帧缓冲 — 通过 Bochs/QEMU PCI VBE 接口初始化。
//!
//! QEMU `-vga std` 提供 Bochs VBE (vendor 0x1234, device 0x1111):
//!   BAR0 = I/O (legacy VGA), BAR1/BAR2 = MMIO LFB
//!   DISPI 寄存器通过 I/O ports 0x1CE(索引)/0x1CF(数据) 访问
//!
//! DISPI 寄存器 (16-bit):
//!   0 = ID (0xB0C0-0xB0C7)
//!   1 = XRES, 2 = YRES, 3 = BPP
//!   4 = ENABLE (0x41 = enable + LFB)

use crate::mm::paging;
use core::ptr::{read_volatile, write_volatile};

#[derive(Clone, Copy)]
pub struct FbInfo {
    pub phys_base: u64, pub width: u32, pub height: u32,
    pub bpp: u8, pub pitch: u32, pub valid: bool,
}

const FB_INFO_ADDR: usize = 0x9C000;

pub fn init(info: FbInfo) {
    if !info.valid { return; }
    unsafe { core::ptr::write_volatile(FB_INFO_ADDR as *mut FbInfo, info); }
    crate::sprintln!("fb: {}x{}x{} @ {:#x}", info.width, info.height, info.bpp, info.phys_base);
}

pub fn info() -> Option<FbInfo> {
    unsafe {
        let fb = core::ptr::read_volatile(FB_INFO_ADDR as *const FbInfo);
        if fb.valid { Some(fb) } else { None }
    }
}

struct BochsVbe { lfb: usize }

impl BochsVbe {
    unsafe fn probe() -> Option<Self> {
        let devices = crate::pci::pci_enumerate();
        for opt in devices.iter() {
            if let Some(dev) = opt {
                if dev.vendor_id != 0x1234 || dev.device_id != 0x1111 { continue; }
                let lfb_bar = dev.bars[2].or(dev.bars[1])?;
                if lfb_bar.is_io { continue; }
                return Some(BochsVbe { lfb: lfb_bar.base as usize });
            }
        }
        None
    }

    unsafe fn dispi_read(index: u16) -> u16 {
        unsafe {
            core::arch::asm!("out dx, ax", in("dx") 0x1CEu16, in("ax") index,
                options(nostack, nomem, preserves_flags));
            let v: u16;
            core::arch::asm!("in ax, dx", in("dx") 0x1CFu16, lateout("ax") v,
                options(nostack, nomem, preserves_flags));
            v
        }
    }

    unsafe fn dispi_write(index: u16, value: u16) {
        unsafe {
            core::arch::asm!("out dx, ax", in("dx") 0x1CEu16, in("ax") index,
                options(nostack, nomem, preserves_flags));
            core::arch::asm!("out dx, ax", in("dx") 0x1CFu16, in("ax") value,
                options(nostack, nomem, preserves_flags));
        }
    }

    unsafe fn init(&self) -> Option<FbInfo> {
        let id = Self::dispi_read(0);
        if id < 0xB0C0 || id > 0xB0C7 { return None; }

        let w = Self::dispi_read(1) as u32;
        let h = Self::dispi_read(2) as u32;
        let bpp = Self::dispi_read(3) as u8;

        if w == 0 || h == 0 {
            Self::dispi_write(1, 1024);
            Self::dispi_write(2, 768);
            Self::dispi_write(3, 32);
            Self::dispi_write(4, 0x41); // ENABLE | LFB_ENABLE
        }

        Some(FbInfo {
            phys_base: self.lfb as u64,
            width: w.max(Self::dispi_read(1) as u32),
            height: h.max(Self::dispi_read(2) as u32),
            bpp: if bpp == 0 { Self::dispi_read(3) as u8 } else { bpp },
            pitch: 0, // computed below
            valid: true,
        })
    }
}

pub fn auto_init() {
    unsafe {
        if let Some(vbe) = BochsVbe::probe() {
            if let Some(mut info) = vbe.init() {
                info.pitch = info.width * (info.bpp as u32 / 8);
                crate::sprintln!("fb: Bochs VBE {}x{}x{} LFB={:#x}",
                    info.width, info.height, info.bpp, info.phys_base);
                init(info);
                selftest(&info);
            }
        }
    }
}

/// 帧缓冲自检：绘制彩色竖条和横条，验证像素渲染
fn selftest(info: &FbInfo) {
    let mut fb = match Framebuffer::map(info) {
        Some(f) => f,
        None => { crate::sprintln!("fb: map failed, skip selftest"); return; }
    };
    let w = info.width;
    let h = info.height;

    // 1. 全屏深蓝背景
    fb.clear(0x00336699);

    // 2. 顶部红色横条 (1/8 屏高)
    let bar_h = h / 8;
    for y in 0..bar_h {
        for x in 0..w {
            fb.put_pixel(x, y, 0x00CC3333); // red
        }
    }

    // 3. 底部绿色横条
    for y in (h - bar_h)..h {
        for x in 0..w {
            fb.put_pixel(x, y, 0x0033CC33); // green
        }
    }

    // 4. 左侧蓝色竖条 (1/8 屏宽)
    let bar_w = w / 8;
    for y in bar_h..(h - bar_h) {
        for x in 0..bar_w {
            fb.put_pixel(x, y, 0x003333CC); // blue
        }
    }

    // 5. 右侧白色竖条
    for y in bar_h..(h - bar_h) {
        for x in (w - bar_w)..w {
            fb.put_pixel(x, y, 0x00CCCCCC); // white
        }
    }

    // 6. 中心区域：渐变横条 (从黄到青)
    let mid_y = h / 2 - bar_h;
    for y in 0..(bar_h * 2) {
        let g = 0x80 + (y * 0x60 / (bar_h * 2)) as u32;
        let color = 0x00CC0000 | (g << 8);
        for x in bar_w..(w - bar_w) {
            fb.put_pixel(x, mid_y + y, color);
        }
    }

    // 7. 绘制一个白色 "X" 标记在中心
    let cx = w / 2;
    let cy = h / 2;
    for i in 0..100i32 {
        let i = i as u32;
        fb.put_pixel(cx - 50 + i, cy - 50 + i, 0x00FFFFFF);
        fb.put_pixel(cx - 50 + i, cy + 50 - i, 0x00FFFFFF);
    }

    // 8. Read back and verify: check a few pixel values from mapped FB
    let base = fb.base;
    let pitch = fb.pitch as isize;
    // Top bar center should be red (0x00CC3333 → B,G,R = 0x33,0x33,0xCC)
    let off = (20isize * pitch + 512 * 4) as usize;
    let pixel = unsafe { core::ptr::read_volatile(base.add(off) as *const u32) };
    crate::sprintln!("fb: top bar pixel = {:#010x} (expect 0x00cc3333)", pixel);
    // Background should be dark blue
    let off = (200isize * pitch + 256 * 4) as usize;
    let pixel = unsafe { core::ptr::read_volatile(base.add(off) as *const u32) };
    crate::sprintln!("fb: bg pixel     = {:#010x} (expect 0x00336699)", pixel);
    // Center X should be white
    let off = ((fb.height/2) as isize * pitch + (fb.width/2 * 4) as isize) as usize;
    let pixel = unsafe { core::ptr::read_volatile(base.add(off) as *const u32) };
    crate::sprintln!("fb: center pixel = {:#010x} (expect 0x00ffffff)", pixel);

    crate::sprintln!("fb: selftest done — check pixel readback above");
}

pub struct Framebuffer {
    pub base: *mut u8, pub width: u32, pub height: u32,
    pub bpp: u8, pub pitch: u32,
}

impl Framebuffer {
    pub fn map(info: &FbInfo) -> Option<Self> {
        let pages = (info.pitch as usize * info.height as usize + 0xFFF) / 0x1000;
        // 必须避开 PHYS_MAP_BASE（0xffff_8000_0000_0000）：那里是物理 0..1GB 的
        // 高半区直接映射，页表（entry/set_entry）和页分配器的空闲链表指针都靠
        // `PHYS_MAP_BASE + phys` 访问。把 LFB 映到 PHYS_MAP_BASE 会覆盖掉低 3MB
        // 的直接映射（物理 0x4000 页表、mem_map 等），后续 map_page 的页表访问
        // 就撞进帧缓冲 → 卡死。选一个独立的高半区规范地址。
        let vaddr = 0xFFFF_9000_0000_0000usize;
        let pml4 = paging::current_pml4();
        if pml4 == 0 { return None; }
        unsafe {
            for i in 0..pages {
                paging::map_page(pml4, vaddr + i * 0x1000,
                    info.phys_base as usize + i * 0x1000, paging::flags::KERNEL);
            }
        }
        Some(Framebuffer { base: vaddr as *mut u8,
            width: info.width, height: info.height, bpp: info.bpp, pitch: info.pitch })
    }

    pub fn put_pixel(&mut self, x: u32, y: u32, color: u32) {
        if x >= self.width || y >= self.height { return; }
        let off = y as usize * self.pitch as usize + x as usize * (self.bpp as usize / 8);
        unsafe { write_volatile(self.base.add(off) as *mut u32, color); }
    }

    pub fn clear(&mut self, color: u32) {
        for y in 0..self.height {
            for x in 0..self.width { self.put_pixel(x, y, color); }
        }
    }
}
