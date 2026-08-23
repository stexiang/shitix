//! UHCI (Universal Host Controller Interface) USB 1.1 驱动。
//!
//! QEMU `-usb` 的默认 USB 控制器（PIIX3，PCI class 0x0C/0x03/0x00，
//! I/O BAR4）。实现了：
//!
//! - 控制器复位 / 帧列表 / 端口复位使能
//! - 控制传输（SETUP → DATA → STATUS 三段 TD 链，轮询完成位）
//! - 设备枚举（GET_DESCRIPTOR → SET_ADDRESS → GET_CONFIGURATION →
//!   SET_CONFIGURATION → HID SET_PROTOCOL(boot)）
//! - HID 键盘：中断 IN 端点轮询（由 idle 循环调
//!   [`poll_keyboard`]），boot 协议 8 字节报告 → ASCII → tty。
//!
//! 传输用「临时挂链」模型：帧列表常态全 T（空），发起传输时把
//! 0 号帧指针指向本次的 QH，轮询最后一个 TD 的 Active 位清零后
//! 摘链。单核 + 传输全程关中断，帧列表不会被并发改。

use crate::mm::{get_free_page, free_page};
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

// USBCMD 位
const CMD_RS: u16 = 1 << 0;      // Run/Stop
const CMD_HCRESET: u16 = 1 << 1; // Host Controller Reset
const CMD_GRESET: u16 = 1 << 2;  // Global Reset

// USBSTS 位（写 1 清除）
const STS_USBINT: u16 = 1 << 0;
const STS_ERROR: u16 = 1 << 1;
const STS_RD: u16 = 1 << 2;      // Resume Detect
const STS_HSE: u16 = 1 << 3;     // Host System Error
const STS_HCPE: u16 = 1 << 4;    // Host Controller Process Error
const STS_HCHALTED: u16 = 1 << 5;

// PORTSC 位
const PORT_CCS: u16 = 1 << 0;    // Current Connect Status
const PORT_CSC: u16 = 1 << 1;    // Connect Status Change（写 1 清）
const PORT_PE: u16 = 1 << 2;     // Port Enable
const PORT_PEC: u16 = 1 << 3;    // Port Enable Change（写 1 清）
const PORT_PR: u16 = 1 << 9;     // Port Reset

// 帧列表 / TD 链接指针位
const LINK_T: u32 = 1 << 0; // Terminate
const LINK_Q: u32 = 1 << 1; // QH（0 = TD）

// TD control/status 位
const TD_SPD: u32 = 1 << 29;
const TD_C_ERR: u32 = 3 << 27; // 错误计数（3 = 允许重试 3 次）
const TD_IOC: u32 = 1 << 24;
const TD_ACTIVE: u32 = 1 << 23;
const TD_STALLED: u32 = 1 << 22;
const TD_ERR_MASK: u32 = 0x7E << 16; // Stalled..Bitstuff
const TD_ACTLEN_MASK: u32 = 0x7FF;

// PID
const PID_OUT: u32 = 0xE1;
const PID_IN: u32 = 0x69;
const PID_SETUP: u32 = 0x2D;

/// Transfer Descriptor（32 字节，16 字节对齐）
#[repr(C, align(16))]
struct Td {
    link: u32,
    status: u32,
    token: u32,
    buffer: u32,
    _reserved: [u32; 4],
}

/// Queue Head（16 字节对齐）
#[repr(C, align(16))]
struct Qh {
    head: u32,
    element: u32,
    _reserved: [u32; 2],
}

/// USB 标准请求 setup packet（8 字节）
#[repr(C, packed)]
struct SetupPacket {
    bm_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    w_length: u16,
}

/// 一次传输用的 TD 池页：QH 在页首，TD 紧随其后。控制传输最多
/// 1 个 SETUP + 8 个 DATA + 1 个 STATUS = 10 个 TD（数据 <= 512B、
/// 包长 64B 封顶；枚举阶段的描述符都远小于此）。
const MAX_TDS: usize = 12;

/// 传输工作区（一页）：[QH][TD×MAX_TDS][数据缓冲]
struct Xfer {
    page: usize,
}

impl Xfer {
    fn alloc() -> Option<Xfer> {
        let page = get_free_page();
        if page == 0 {
            return None;
        }
        // SAFETY: 刚分配的页，恒等映射可写。
        unsafe { core::ptr::write_bytes(page as *mut u8, 0, 4096) };
        Some(Xfer { page })
    }

    fn qh(&mut self) -> *mut Qh {
        self.page as *mut Qh
    }

    fn td(&mut self, i: usize) -> *mut Td {
        (self.page + 16 + i * 32) as *mut Td
    }

    /// 数据缓冲起点（页内偏移 512，留足 QH+TD 区）
    fn data(&mut self) -> *mut u8 {
        (self.page + 512) as *mut u8
    }

    fn data_capacity(&self) -> usize {
        4096 - 512
    }
}

impl Drop for Xfer {
    fn drop(&mut self) {
        free_page(self.page);
    }
}

fn td_fill(td: *mut Td, link: u32, pid: u32, dev: u8, ep: u8, toggle: u32, buf: u32, len: usize) {
    // 零长度包按规范编码为 MaxLen = 0x7FF
    let maxlen = if len == 0 { 0x7FF } else { (len as u32 - 1) & 0x7FF };
    let token = (maxlen << 21)
        | ((toggle & 1) << 19)
        | (((ep as u32) & 0xF) << 15)
        | (((dev as u32) & 0x7F) << 8)
        | pid;
    // SAFETY: td 指向 Xfer 页内合法槽位。
    unsafe {
        write_volatile(&mut (*td).link, link);
        write_volatile(&mut (*td).status, TD_ACTIVE | TD_C_ERR | TD_IOC | TD_SPD);
        write_volatile(&mut (*td).token, token);
        write_volatile(&mut (*td).buffer, buf);
    }
}

pub struct Uhci {
    base: u16,         // I/O port base
    frame_list: usize, // 4KB 页：1024 个帧指针
    enabled: bool,
}

static mut UHCI_DEV: Option<Uhci> = None;

/// 已枚举出的 HID 键盘
struct HidKb {
    dev_addr: u8,
    ep: u8,       // 中断 IN 端点号（不含方向位）
    max_packet: u16,
    toggle: u32,  // 中断 IN 的 data toggle
    prev_report: [u8; 8],
    present: bool,
}

static mut HID_KB: HidKb = HidKb {
    dev_addr: 0,
    ep: 0,
    max_packet: 8,
    toggle: 0,
    prev_report: [0; 8],
    present: false,
};

impl Uhci {
    fn inw(&self, port: u16) -> u16 {
        // SAFETY: base 是 probe 来的有效 UHCI I/O BAR。
        unsafe {
            let val: u16;
            core::arch::asm!("in ax, dx", in("dx") self.base + port, out("ax") val,
                options(nostack, nomem, preserves_flags));
            val
        }
    }

    fn outw(&self, port: u16, val: u16) {
        // SAFETY: 同 inw。
        unsafe {
            core::arch::asm!("out dx, ax", in("dx") self.base + port, in("ax") val,
                options(nostack, nomem, preserves_flags));
        }
    }

    fn inl(&self, port: u16) -> u32 {
        // SAFETY: 同 inw。
        unsafe {
            let val: u32;
            core::arch::asm!("in eax, dx", in("dx") self.base + port, out("eax") val,
                options(nostack, nomem, preserves_flags));
            val
        }
    }

    fn outl(&self, port: u16, val: u32) {
        // SAFETY: 同 inw。
        unsafe {
            core::arch::asm!("out dx, eax", in("dx") self.base + port, in("eax") val,
                options(nostack, nomem, preserves_flags));
        }
    }

    /// 通过 PCI 探测 UHCI 控制器 (class=0x0C, subclass=0x03, prog_if=0x00)
    pub fn probe() -> Option<u16> {
        let devices = crate::pci::pci_enumerate();
        for opt in devices.iter() {
            if let Some(dev) = opt {
                if dev.class_code == 0x0C && dev.subclass == 0x03 && dev.prog_if == 0x00 {
                    let bar = dev.bars[4]?; // UHCI uses BAR4 for I/O ports
                    if !bar.is_io {
                        continue;
                    }
                    return Some(bar.base as u16);
                }
            }
        }
        None
    }

    fn init(base: u16) -> Option<&'static mut Uhci> {
        // SAFETY: 启动期单线程，UHCI_DEV 只在这里写一次。
        unsafe {
            let frame_page = get_free_page();
            if frame_page == 0 {
                return None;
            }
            // 帧列表初始化为全 Terminate（空调度）；传输时临时挂 0 号帧。
            core::ptr::write_bytes(frame_page as *mut u8, 0xFF, 4096); // 0xFFFFFFFF 也是 T=1
            for i in 0..1024 {
                write_volatile((frame_page as *mut u32).add(i), LINK_T);
            }

            let slot = &raw mut UHCI_DEV;
            *slot = Some(Uhci { base, frame_list: frame_page, enabled: false });
            let dev = (*slot).as_mut().unwrap();

            // 全局复位
            dev.outw(USBCMD, CMD_GRESET);
            spin_us(50_000);
            dev.outw(USBCMD, 0);
            spin_us(1_000);

            // 清状态位（写 1 清），关掉所有中断源（我们轮询）
            dev.outw(USBSTS, STS_USBINT | STS_ERROR | STS_RD | STS_HSE | STS_HCPE);
            dev.outw(USBINTR, 0);

            // 帧列表基址 + 帧号 0 + SOF
            dev.outl(FLBASEADD, frame_page as u32);
            dev.outw(FRNUM, 0);
            // SAFETY: SOFMOD 是 8 位寄存器。
            core::arch::asm!("out dx, al", in("dx") base + SOFMOD, in("al") 64u8,
                options(nostack, nomem, preserves_flags));

            // Run
            dev.outw(USBCMD, CMD_RS);
            let mut waited = 0;
            while dev.inw(USBSTS) & STS_HCHALTED != 0 && waited < 100_000 {
                spin_us(10);
                waited += 1;
            }
            if dev.inw(USBSTS) & STS_HCHALTED != 0 {
                crate::sprintln!("uhci: controller failed to start");
                return None;
            }

            dev.enabled = true;
            crate::sprintln!("uhci: controller at IO {:#x}, running", base);
            Some((*slot).as_mut().unwrap())
        }
    }

    /// 复位并使能一个端口。返回是否有设备接入。
    fn port_reset_enable(&self, port_reg: u16) -> bool {
        let sc = self.inw(port_reg);
        if sc & PORT_CCS == 0 {
            return false;
        }
        // 复位 ≥50ms（USB 2.0 规范 7.1.7.5）
        self.outw(port_reg, PORT_PR);
        spin_us(60_000);
        self.outw(port_reg, 0);
        spin_us(10_000);
        // 清掉变化位（写 1 清 CSC/PEC），再置 Port Enable
        self.outw(port_reg, PORT_CSC | PORT_PEC);
        spin_us(1_000);
        self.outw(port_reg, PORT_PE);
        spin_us(10_000);
        let sc = self.inw(port_reg);
        sc & (PORT_CCS | PORT_PE) == (PORT_CCS | PORT_PE)
    }

    /// 把帧指针 0..8 挂到 `qh`，开始执行。
    fn link_qh(&self, qh_phys: u32) {
        // SAFETY: frame_list 页有效；传输全程关中断，无并发。
        unsafe {
            for i in 0..8 {
                write_volatile((self.frame_list as *mut u32).add(i), qh_phys | LINK_Q);
            }
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }

    fn unlink(&self) {
        // SAFETY: 同 link_qh。
        unsafe {
            for i in 0..8 {
                write_volatile((self.frame_list as *mut u32).add(i), LINK_T);
            }
        }
    }

    /// 执行一条 TD 链（qh.element → 首 TD），等最后一个 TD 的 Active
    /// 清零。返回 Ok(实际收到字节数，只对末段有意义) 或 Err(状态)。
    fn run_chain(&self, xfer: &mut Xfer, last_td: usize, timeout_ms: u32) -> Result<u32, u32> {
        // SAFETY: 传输期间关中断，帧列表/TD 状态不被并发动。
        let irq = unsafe { crate::irq::local_irq_save() };
        self.link_qh(xfer.qh() as u32);

        let deadline = crate::sched::jiffies() + (timeout_ms as u64 * 100 + 999) / 1000 + 2;
        let last = xfer.td(last_td);
        // SAFETY: last 指向本传输的末 TD。
        let status = loop {
            let s = unsafe { read_volatile(&(*last).status) };
            if s & TD_ACTIVE == 0 {
                break s;
            }
            if crate::sched::jiffies() > deadline {
                // 停链摘走
                self.outw(USBCMD, 0); // stop
                self.unlink();
                self.outw(USBCMD, CMD_RS);
                // SAFETY: 与 local_irq_save 配对。
                unsafe { crate::irq::restore_flags(irq) };
                return Err(s);
            }
            core::hint::spin_loop();
        };

        self.unlink();
        // SAFETY: 与 local_irq_save 配对。
        unsafe { crate::irq::restore_flags(irq) };

        if status & (TD_STALLED | TD_ERR_MASK) != 0 {
            return Err(status);
        }
        // Actual Length 编码为 len-1；0x7FF 表示零长度
        let act = status & TD_ACTLEN_MASK;
        Ok(if act == 0x7FF { 0 } else { act + 1 })
    }

    /// 控制传输。`data` 为数据段缓冲（可为 None 表示无数据段），
    /// `dir_in` 为数据段方向。返回数据段实际传输字节数。
    fn control_transfer(
        &self,
        dev: u8,
        mps: u8,
        setup: &SetupPacket,
        data: Option<&mut [u8]>,
        dir_in: bool,
    ) -> Result<usize, ()> {
        let mut xfer = Xfer::alloc().ok_or(())?;

        let data_len = data.as_ref().map_or(0, |d| d.len());
        if data_len > xfer.data_capacity() {
            return Err(());
        }
        let data_phys = xfer.data() as u32;

        // OUT 数据段：先把数据放进 DMA 缓冲
        if let Some(d) = data.as_ref() {
            if !dir_in && data_len > 0 {
                // SAFETY: 缓冲容量已检查。
                unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), xfer.data(), data_len) };
            }
        }

        // 组 TD 链：SETUP(DATA0) → DATA×n(DATA1 起交替) → STATUS(DATA1)
        let ndata = if data_len == 0 { 0 } else { data_len.div_ceil(mps as usize) };
        let ntd = 1 + ndata + 1;
        if ntd > MAX_TDS {
            return Err(());
        }

        // SAFETY: setup 是调用者的 8 字节结构，拷进 DMA 区（数据缓冲前
        // 预留了 512 字节，用其后半放 setup，与数据区不重叠）。
        let setup_phys = (xfer.page + 256) as u32;
        unsafe {
            core::ptr::copy_nonoverlapping(
                setup as *const SetupPacket as *const u8,
                (xfer.page + 256) as *mut u8,
                8,
            );
        }

        let td0 = xfer.td(0);
        let link_to = |x: &Xfer, i: usize| (x.page + 16 + i * 32) as u32; // TD i 物理地址
        td_fill(td0, link_to(&xfer, 1), PID_SETUP, dev, 0, 0, setup_phys, 8);

        for i in 0..ndata {
            let off = i * mps as usize;
            let chunk = core::cmp::min(mps as usize, data_len - off);
            let pid = if dir_in { PID_IN } else { PID_OUT };
            td_fill(
                xfer.td(1 + i),
                link_to(&xfer, 2 + i),
                pid,
                dev,
                0,
                (1 + i) as u32 & 1,
                data_phys + off as u32,
                chunk,
            );
        }

        // STATUS 段：方向与数据段相反（无数据段则 IN），DATA1，零长度
        let status_pid = if dir_in { PID_OUT } else { PID_IN };
        td_fill(
            xfer.td(1 + ndata),
            LINK_T,
            status_pid,
            dev,
            0,
            1,
            0,
            0,
        );

        // QH 指向 SETUP TD
        // SAFETY: xfer 页内。
        unsafe {
            write_volatile(&mut (*xfer.qh()).head, LINK_T);
            write_volatile(&mut (*xfer.qh()).element, link_to(&xfer, 0));
        }

        let r = self.run_chain(&mut xfer, 1 + ndata, 500);
        match r {
            Ok(_) => {
                if dir_in && data_len > 0 {
                    // 统计实际收到的总字节：逐 DATA TD 读 Actual Length
                    let mut total = 0usize;
                    for i in 0..ndata {
                        // SAFETY: TD 已完成。
                        let s = unsafe { read_volatile(&(*xfer.td(1 + i)).status) };
                        let act = s & TD_ACTLEN_MASK;
                        total += if act == 0x7FF { 0 } else { act as usize + 1 };
                    }
                    if let Some(d) = data {
                        let n = core::cmp::min(total, d.len());
                        // SAFETY: 数据在 DMA 缓冲。
                        unsafe { core::ptr::copy_nonoverlapping(xfer.data(), d.as_mut_ptr(), n) };
                        return Ok(n);
                    }
                    return Ok(total);
                }
                Ok(0)
            }
            Err(_) => Err(()),
        }
    }

    /// 中断 IN 传输（单 TD）。返回读到的字节数；NAK/超时 → Ok(0)。
    fn interrupt_in(&self, dev: u8, ep: u8, toggle: u32, buf: &mut [u8], mps: u16) -> Result<usize, ()> {
        let mut xfer = Xfer::alloc().ok_or(())?;
        let len = core::cmp::min(buf.len(), mps as usize);
        td_fill(
            xfer.td(0),
            LINK_T,
            PID_IN,
            dev,
            ep,
            toggle & 1,
            xfer.data() as u32,
            len,
        );
        // SAFETY: xfer 页内。
        unsafe {
            write_volatile(&mut (*xfer.qh()).head, LINK_T);
            write_volatile(&mut (*xfer.qh()).element, xfer.td(0) as u32);
        }
        match self.run_chain(&mut xfer, 0, 50) {
            Ok(n) => {
                let n = core::cmp::min(n as usize, buf.len());
                // SAFETY: 数据在 DMA 缓冲。
                unsafe { core::ptr::copy_nonoverlapping(xfer.data(), buf.as_mut_ptr(), n) };
                Ok(n)
            }
            Err(_) => Err(()),
        }
    }
}

/// 粗粒度忙等（微秒量级，启动期枚举用；1000 次空转 ≈ 1us 量级）
fn spin_us(us: u32) {
    for _ in 0..us * 4 {
        core::hint::spin_loop();
    }
}

fn std_get_descriptor(dev: u8, mps: u8, dtype: u8, index: u8, buf: &mut [u8]) -> Result<usize, ()> {
    let hc = match get() { Some(h) => h, None => return Err(()) };
    let setup = SetupPacket {
        bm_request_type: 0x80, // IN, standard, device
        b_request: 6,          // GET_DESCRIPTOR
        w_value: ((dtype as u16) << 8) | index as u16,
        w_index: 0,
        w_length: buf.len() as u16,
    };
    hc.control_transfer(dev, mps, &setup, Some(buf), true)
}

fn std_set_address(dev_new: u8) -> Result<(), ()> {
    let hc = get().ok_or(())?;
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 5, // SET_ADDRESS
        w_value: dev_new as u16,
        w_index: 0,
        w_length: 0,
    };
    hc.control_transfer(0, 8, &setup, None, false).map(|_| ())
}

fn std_set_configuration(dev: u8, mps: u8, cfg: u8) -> Result<(), ()> {
    let hc = get().ok_or(())?;
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 9, // SET_CONFIGURATION
        w_value: cfg as u16,
        w_index: 0,
        w_length: 0,
    };
    hc.control_transfer(dev, mps, &setup, None, false).map(|_| ())
}

/// HID class 请求 SET_PROTOCOL（0 = boot protocol）
fn hid_set_boot_protocol(dev: u8, mps: u8, interface: u8) -> Result<(), ()> {
    let hc = get().ok_or(())?;
    let setup = SetupPacket {
        bm_request_type: 0x21, // OUT, class, interface
        b_request: 0x0B,       // SET_PROTOCOL
        w_value: 0,            // boot
        w_index: interface as u16,
        w_length: 0,
    };
    hc.control_transfer(dev, mps, &setup, None, false).map(|_| ())
}

/// 枚举一个端口上的设备；是 boot 键盘则登记到 HID_KB 并返回 true。
fn enumerate_port(hc: &Uhci, port_reg: u16) -> bool {
    if !hc.port_reset_enable(port_reg) {
        return false;
    }
    crate::sprintln!("uhci: device attached on port, enumerating");

    // 1. 先读 8 字节设备描述符拿 bMaxPacketSize0（地址 0 阶段一律用 8 包长）
    let mut desc8 = [0u8; 8];
    if std_get_descriptor(0, 8, 1, 0, &mut desc8).is_err() {
        crate::sprintln!("uhci: GET_DESCRIPTOR(8) failed");
        return false;
    }
    let mps = desc8[7];
    let mps = if mps == 0 { 8 } else { mps };

    // 2. SET_ADDRESS
    if std_set_address(1).is_err() {
        crate::sprintln!("uhci: SET_ADDRESS failed");
        return false;
    }
    spin_us(2_000); // 设备有 2ms 完成地址切换

    // 3. 完整设备描述符
    let mut dev_desc = [0u8; 18];
    if std_get_descriptor(1, mps, 1, 0, &mut dev_desc).is_err() {
        crate::sprintln!("uhci: GET_DESCRIPTOR(device) failed");
        return false;
    }
    let vid = u16::from_le_bytes([dev_desc[8], dev_desc[9]]);
    let pid = u16::from_le_bytes([dev_desc[10], dev_desc[11]]);
    crate::sprintln!("uhci: device {:04x}:{:04x} at addr 1 (mps={})", vid, pid, mps);

    // 4. 配置描述符（先 9 字节拿总长）
    let mut cfg_head = [0u8; 9];
    if std_get_descriptor(1, mps, 2, 0, &mut cfg_head).is_err() {
        return false;
    }
    let total = u16::from_le_bytes([cfg_head[2], cfg_head[3]]) as usize;
    let mut cfg_buf = [0u8; 512];
    let want = core::cmp::min(total, cfg_buf.len());
    if std_get_descriptor(1, mps, 2, 0, &mut cfg_buf[..want]).is_err() {
        return false;
    }

    // 5. 扫描述符找 HID boot-keyboard 接口 + 中断 IN 端点
    let mut cfg_val = cfg_buf[5];
    let mut ifc: Option<(u8, u8)> = None; // (interface number, protocol)
    let mut ep: Option<(u8, u16)> = None; // (ep num, mps)
    let mut cur_hid: Option<u8> = None;
    let mut i = cfg_buf[0] as usize;
    while i + 2 <= want {
        let len = cfg_buf[i] as usize;
        if len < 2 || i + len > want {
            break;
        }
        let dtype = cfg_buf[i + 1];
        match dtype {
            4 => {
                // INTERFACE
                let num = cfg_buf[i + 2];
                let class = cfg_buf[i + 5];
                let subclass = cfg_buf[i + 6];
                let proto = cfg_buf[i + 7];
                cur_hid = if class == 0x03 && (subclass == 0x01 || proto == 0x01) {
                    ifc = Some((num, proto));
                    Some(num)
                } else {
                    None
                };
            }
            5 => {
                // ENDPOINT
                if cur_hid.is_some() && ep.is_none() {
                    let addr = cfg_buf[i + 2];
                    let attr = cfg_buf[i + 3] & 0x03;
                    let m = u16::from_le_bytes([cfg_buf[i + 4], cfg_buf[i + 5]]);
                    if addr & 0x80 != 0 && attr == 3 {
                        ep = Some((addr & 0x0F, m));
                    }
                }
            }
            _ => {}
        }
        i += len;
    }

    let (ifnum, _proto) = match ifc {
        Some(v) => v,
        None => {
            crate::sprintln!("uhci: not a boot HID keyboard, skipping");
            return false;
        }
    };
    let (epnum, epmps) = match ep {
        Some(v) => v,
        None => {
            crate::sprintln!("uhci: no interrupt IN endpoint");
            return false;
        }
    };

    if cfg_val == 0 {
        cfg_val = 1;
    }
    if std_set_configuration(1, mps, cfg_val).is_err() {
        crate::sprintln!("uhci: SET_CONFIGURATION failed");
        return false;
    }
    // boot protocol（失败不致命：QEMU usb-kbd 默认就是 boot 协议）
    let _ = hid_set_boot_protocol(1, mps, ifnum);

    // SAFETY: 启动期单线程。
    unsafe {
        let kb = &mut *core::ptr::addr_of_mut!(HID_KB);
        kb.dev_addr = 1;
        kb.ep = epnum;
        kb.max_packet = core::cmp::min(epmps, 8);
        kb.toggle = 0;
        kb.prev_report = [0; 8];
        kb.present = true;
    }
    crate::sprintln!(
        "uhci: HID keyboard addr=1 if={} ep={} mps={} (boot protocol)",
        ifnum, epnum, epmps
    );
    true
}

/// 获取控制器（已初始化时）
pub fn get() -> Option<&'static Uhci> {
    // SAFETY: 只读；写只发生在 init。
    unsafe {
        let p = &raw const UHCI_DEV;
        (*p).as_ref()
    }
}

/// 初始化 UHCI 并枚举 HID 键盘。启动期由 `drivers::init` 调用
/// （extra-drivers）。找不到控制器/键盘都静默跳过（PS/2 键盘仍在）。
pub fn init() {
    let Some(base) = Uhci::probe() else {
        return;
    };
    let Some(hc) = Uhci::init(base) else {
        return;
    };
    // 两个端口都试
    for port in [PORTSC1, PORTSC2] {
        if enumerate_port(hc, port) {
            break;
        }
    }
    selftest();
}

/// 重新扫描两个根端口。已经枚举到 HID 键盘时不动（重复枚举会换设备
/// 地址，把正在用的键盘状态打掉）；还没枚举到时补扫一次——供
/// `usb::scan_ports` 在热插/延迟接入场景调用。
pub fn rescan_ports() {
    if keyboard_present() {
        return;
    }
    if let Some(hc) = get() {
        for port in [PORTSC1, PORTSC2] {
            if enumerate_port(hc, port) {
                break;
            }
        }
    }
}

/// 周期轮询 HID 键盘（由 idle 循环每次唤醒调一次，≈100Hz）。无键盘/无
/// 数据时立即返回，成本是一次端口 I/O 级判空。
pub fn poll_keyboard() {
    // SAFETY: 只读全局状态；poll 只在时钟中断里跑，单核不重入
    // （do_timer 是 fast interrupt，全程关中断）。
    let (dev, ep, toggle, mps) = unsafe {
        let kb = &*core::ptr::addr_of!(HID_KB);
        if !kb.present {
            return;
        }
        (kb.dev_addr, kb.ep, kb.toggle, kb.max_packet)
    };
    let Some(hc) = get() else { return };

    let mut report = [0u8; 8];
    let n = match hc.interrupt_in(dev, ep, toggle, &mut report, mps) {
        Ok(n) => n,
        Err(_) => return,
    };
    if n == 0 {
        return;
    }
    // SAFETY: 单核时钟中断上下文，独占 HID_KB。
    unsafe {
        let kb = &mut *core::ptr::addr_of_mut!(HID_KB);
        kb.toggle ^= 1;
        handle_report(&mut kb.prev_report, &report);
    }
}

/// 对比前后两份 boot 报告，对新按下的键生成字符送 tty。
/// （只在按下沿发字符，不做自动重复；松开沿只更新修饰键。）
fn handle_report(prev: &mut [u8; 8], cur: &[u8; 8]) {
    let shift = cur[0] & 0x22 != 0; // LShift|RShift
    let ctrl = cur[0] & 0x11 != 0;  // LCtrl|RCtrl

    for &code in &cur[2..8] {
        if code == 0 || prev[2..8].contains(&code) {
            continue;
        }
        let Some(mut ch) = hid_to_ascii(code, shift) else { continue };
        if ctrl && ch.is_ascii_alphabetic() {
            ch = ch.to_ascii_uppercase() & 0x1f;
        }
        // SAFETY: 时钟中断上下文（关中断），与键盘 IRQ 同契约。
        unsafe { crate::drivers::char_dev::tty::receive_char(ch) };
    }
    *prev = *cur;
}

/// 将 USB HID usage ID 转换为 ASCII（US 键盘布局）
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
        0x2A => Some(0x08),  // Backspace
        0x2B => Some(b'\t'), // Tab
        0x2C => Some(b' '),  // Space
        0x2D => Some(if shift { b'_' } else { b'-' }),
        0x2E => Some(if shift { b'+' } else { b'=' }),
        0x2F => Some(if shift { b'{' } else { b'[' }),
        0x30 => Some(if shift { b'}' } else { b']' }),
        0x31 => Some(if shift { b'|' } else { b'\\' }),
        0x33 => Some(if shift { b':' } else { b';' }),
        0x34 => Some(if shift { b'"' } else { b'\'' }),
        0x35 => Some(if shift { b'~' } else { b'`' }),
        0x36 => Some(if shift { b'<' } else { b',' }),
        0x37 => Some(if shift { b'>' } else { b'.' }),
        0x38 => Some(if shift { b'?' } else { b'/' }),
        _ => None,
    }
}

/// HID 键盘是否已枚举（自检用）
pub fn keyboard_present() -> bool {
    // SAFETY: 只读。
    unsafe { (*core::ptr::addr_of!(HID_KB)).present }
}

/// 自检：hid_to_ascii 映射（纯函数，不碰 tty）。
pub fn selftest() {
    let ok = hid_to_ascii(0x04, false) == Some(b'a')
        && hid_to_ascii(0x04, true) == Some(b'A')
        && hid_to_ascii(0x28, false) == Some(b'\n')
        && hid_to_ascii(0x2A, false) == Some(0x08)
        && hid_to_ascii(0x2C, false) == Some(b' ')
        && hid_to_ascii(0x27, true) == Some(b')')
        && hid_to_ascii(0xE0, false).is_none(); // 修饰键不产字符
    crate::kprintln!("uhci: hid keymap selftest -> {}", if ok { "ok" } else { "FAIL" });
}
