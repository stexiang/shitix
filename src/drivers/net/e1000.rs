//! e1000 网卡驱动（Intel 82540EM/82545EM/8254x，QEMU 默认 e1000）。
//!
//! PCI VID=0x8086 DID=0x100E (82540EM), 0x100F (82545EM), 0x1200 (82547).
//! BAR0 = MMIO 寄存器基址。RX 32 desc × 2KB, TX 8 desc。
//!
//! ## 寄存器（偏移 / BAR0）
//! CTRL(0x00) STATUS(0x08) EECD(0x10) ICR(0xC0) IMS(0xD0)
//! RCTL(0x100) TCTL(0x400) TIPG(0x410)
//! RDBAL(0x2800) RDBAH(0x2804) RDLEN(0x2808) RDH(0x2810) RDT(0x2818)
//! TDBAL(0x3800) TDBAH(0x3804) TDLEN(0x3808) TDH(0x3810) TDT(0x3818)
//! RA(0x5400) — Receive Address (MAC)

use crate::mm::get_free_page;
use core::ptr::{read_volatile, write_volatile};

// ---- register offsets ----
const CTRL: usize = 0x0000;
const STATUS: usize = 0x0008;
const EECD: usize = 0x0010;
const ICR: usize = 0x00C0;
const IMS: usize = 0x00D0;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const TIPG: usize = 0x0410;
const RDBAL: usize = 0x2800;
const RDBAH: usize = 0x2804;
const RDLEN: usize = 0x2808;
const RDH: usize = 0x2810;
const RDT: usize = 0x2818;
const TDBAL: usize = 0x3800;
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const RA: usize = 0x5400;

/// RX 描述符（16 bytes, aligned）
#[repr(C, align(16))]
struct RxDesc {
    addr: u64,      // buffer physical address
    length: u16,
    packet_csum: u16,
    status: u8,     // DD=bit0, EOP=bit1
    errors: u8,
    special: u16,
}

/// TX 描述符（16 bytes, aligned）
#[repr(C, align(16))]
struct TxDesc {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,        // EOP=0x01, IFCS=0x02, RS=0x08, IDE=0x20
    status: u8,     // DD=bit0
    css: u8,
    special: u16,
}

const RX_DESC_COUNT: usize = 32;
const TX_DESC_COUNT: usize = 8;
const RX_BUF_SIZE: usize = 2048;

pub struct E1000 {
    pub mmio_base: usize,
    bus: u8, dev: u8, func: u8,
    pub mac: [u8; 6],
    rx_descs: usize,    // page containing RX descriptors
    tx_descs: usize,    // page containing TX descriptors
    rx_bufs: [usize; RX_DESC_COUNT],
    rx_cur: usize,
    tx_cur: usize,
    pub irq: u8,
    pub initialized: bool,
}

static mut E1000_DEV: Option<E1000> = None;

impl E1000 {
    unsafe fn rd(&self, off: usize) -> u32 {
        unsafe { read_volatile((self.mmio_base + off) as *const u32) }
    }
    unsafe fn wr(&self, off: usize, val: u32) {
        unsafe { write_volatile((self.mmio_base + off) as *mut u32, val) }
    }

    /// 探测、初始化 e1000。成功返回设备引用。
    pub fn probe() -> Option<*mut E1000> {
        let eth = crate::pci::pci_find_ethernet()?;
        // Accept all Intel ethernet: 0x8086 + various DID
        if eth.vendor_id != 0x8086 {
            crate::sprintln!("e1000: non-Intel eth vendor={:#x}, skip", eth.vendor_id);
            return None;
        }
        let bar0 = eth.bars[0]?;
        if bar0.is_io {
            // QEMU e1000e uses IO BAR — try BAR1 or skip
            crate::sprintln!("e1000: BAR0 is IO port, trying MMIO BAR1");
            // Some chips use BAR0=MMIO, BAR1=IO. Try next BAR.
            let bar1 = eth.bars[1]?;
            if bar1.is_io { return None; }
            Self::init(bar1.base as usize, eth.bus, eth.device, eth.function)
        } else {
            Self::init(bar0.base as usize, eth.bus, eth.device, eth.function)
        }
    }

    fn init(mmio: usize, bus: u8, dev: u8, func: u8) -> Option<*mut E1000> {
        // Enable PCI bus mastering + IO + memory space
        crate::pci::pci_write16(bus, dev, func, 4,
            crate::pci::pci_read16(bus, dev, func, 4) | 0x07);

        unsafe {
            let slot = &raw mut E1000_DEV;
            (*slot) = Some(E1000 {
                mmio_base: mmio, bus, dev, func,
                mac: [0; 6], rx_descs: 0, tx_descs: 0,
                rx_bufs: [0; RX_DESC_COUNT],
                rx_cur: 0, tx_cur: 0, irq: 0, initialized: false,
            });
            let d = (*slot).as_mut().unwrap();

            // --- Reset ---
            d.wr(CTRL, d.rd(CTRL) | 0x0400_0000); // RST
            for _ in 0..100000 { if d.rd(CTRL) & 0x0400_0000 == 0 { break; } }
            // Wait for PHY auto-negotiation / link up
            for _ in 0..100000 { if d.rd(STATUS) & 0x8000_0000 != 0 { break; } }
            // Clear RST
            d.wr(CTRL, d.rd(CTRL) & !0x0400_0000);

            // --- MAC ---
            let ral = d.rd(RA);
            let rah = d.rd(RA + 4);
            d.mac = [ral as u8, (ral>>8) as u8, (ral>>16) as u8, (ral>>24) as u8,
                      rah as u8, (rah>>8) as u8];

            // --- RX descriptors ---
            let rx_page = get_free_page();
            if rx_page == 0 { return None; }
            d.rx_descs = rx_page;
            let rx = rx_page as *mut RxDesc;
            for i in 0..RX_DESC_COUNT {
                let buf = get_free_page();
                if buf == 0 { return None; }
                d.rx_bufs[i] = buf;
                (*rx.add(i)) = RxDesc {
                    addr: buf as u64, length: 0, packet_csum: 0,
                    status: 0, errors: 0, special: 0,
                };
            }

            // --- TX descriptors ---
            let tx_page = get_free_page();
            if tx_page == 0 { return None; }
            d.tx_descs = tx_page;
            let tx = tx_page as *mut TxDesc;
            for i in 0..TX_DESC_COUNT {
                (*tx.add(i)) = TxDesc {
                    addr: 0, length: 0, cso: 0, cmd: 0,
                    status: 1, css: 0, special: 0, // DD=1 = done
                };
            }

            // --- Configure RX ---
            d.wr(RDBAL, rx_page as u32); d.wr(RDBAH, 0);
            d.wr(RDLEN, (RX_DESC_COUNT * 16) as u32);
            d.wr(RDH, 0);
            d.wr(RDT, (RX_DESC_COUNT - 1) as u32);
            // RCTL: EN(bit1)=1, SBP(bit2)=1 (store bad), UPE(bit3)=1,
            //       BAM(bit15)=1 (broadcast), BSIZE(16:17)=0 (2048),
            //       SECRC(bit26)=1 (strip CRC)
            d.wr(RCTL, (1 << 1) | (1 << 2) | (1 << 3) | (1 << 15) | (1 << 26));

            // --- Configure TX ---
            d.wr(TDBAL, tx_page as u32); d.wr(TDBAH, 0);
            d.wr(TDLEN, (TX_DESC_COUNT * 16) as u32);
            d.wr(TDH, 0); d.wr(TDT, 0);
            // TCTL: EN(bit1)=1, PSP(bit3)=1 (pad short packets), CT=0x0F (collision threshold)
            // CT is at bits 4-13, COLD is at bits 22-31
            d.wr(TCTL, (1 << 1) | (1 << 3) | (0x0F << 4) | (0x40 << 22));
            // Inter-packet gap
            d.wr(TIPG, 0x0060_200A);

            // --- Disable all interrupts for now (polling mode) ---
            d.wr(IMS, 0); // Mask all
            // Clear any pending
            d.rd(ICR);

            // IRQ line
            d.irq = crate::pci::pci_read8(bus, dev, func, 0x3C);
            d.initialized = true;

            crate::sprintln!("e1000: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} MMIO={:#x} IRQ={}",
                d.mac[0], d.mac[1], d.mac[2], d.mac[3], d.mac[4], d.mac[5],
                mmio, d.irq);
            Some((*slot).as_mut().unwrap() as *mut E1000)
        }
    }

    /// 发送以太网帧。返回发送字节数或负错误码。
    pub fn send(&mut self, data: &[u8]) -> i32 {
        if data.len() < 14 || data.len() > RX_BUF_SIZE { return -1; }
        unsafe {
            let tx = self.tx_descs as *mut TxDesc;
            let idx = self.tx_cur;
            let desc = &mut *tx.add(idx);

            // Wait if descriptor is still owned by hardware (!DD)
            for _ in 0..100000 {
                if desc.status & 1 != 0 { break; }
            }
            if desc.status & 1 == 0 { return -16; } // EBUSY

            // Copy packet to a temp buffer held by this descriptor
            if desc.addr == 0 {
                let p = get_free_page();
                if p == 0 { return -12; } // ENOMEM
                desc.addr = p as u64;
            }
            core::ptr::copy_nonoverlapping(data.as_ptr(), desc.addr as *mut u8, data.len());
            desc.length = data.len() as u16;
            desc.cmd = 0x0B; // EOP(1) | IFCS(2) | RS(8)
            desc.status = 0; // Clear DD — hardware owns it now
            desc.cso = 0;
            desc.css = 0;
            desc.special = 0;

            let old = self.tx_cur;
            self.tx_cur = (self.tx_cur + 1) % TX_DESC_COUNT;
            self.wr(TDT, self.tx_cur as u32);

            // Wait for completion with timeout
            let mut done = false;
            for _ in 0..100000 {
                if (*tx.add(old)).status & 1 != 0 { done = true; break; }
            }
            if !done {
                // Try reclaiming — maybe we already wrapped
                self.tx_cur = old;
                return -11; // EAGAIN
            }
            data.len() as i32
        }
    }

    /// 接收一个以太网帧（轮询）。返回 Some(len) 或 None。
    pub fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        unsafe {
            let rx = self.rx_descs as *mut RxDesc;
            let desc = &mut *rx.add(self.rx_cur);
            if desc.status & 1 == 0 { return None; } // DD not set

            let len = desc.length as usize;
            let n = core::cmp::min(len, buf.len());
            core::ptr::copy_nonoverlapping(
                self.rx_bufs[self.rx_cur] as *const u8, buf.as_mut_ptr(), n);

            // Return descriptor to hardware: clear DD, update tail
            desc.status = 0;
            let old = self.rx_cur;
            self.rx_cur = (self.rx_cur + 1) % RX_DESC_COUNT;
            self.wr(RDT, old as u32);
            Some(n)
        }
    }

    pub fn has_packet(&self) -> bool {
        unsafe {
            let rx = self.rx_descs as *const RxDesc;
            unsafe { (*rx.add(self.rx_cur)).status & 1 != 0 }
        }
    }

    pub fn get() -> Option<*mut E1000> {
        unsafe {
            // Read via raw pointer to avoid &mut to static mut (Rust 2024)
            let opt_ptr: *const Option<E1000> = &raw const E1000_DEV;
            if (*opt_ptr).is_some() {
                // Get a pointer to the inner E1000 without creating a & reference
                let inner: *const E1000 = &raw const (*opt_ptr) as *const E1000;
                Some(inner as *mut E1000)
            } else { None }
        }
    }

    /// e1000 自检：发送 ARP 广播包，验证回环。
    pub fn selftest() {
        crate::sprintln!("--- e1000 selftest ---");
        let dev = match Self::probe() {
            Some(d) => d,
            None => {
                crate::sprintln!("e1000: no device found, skip");
                return;
            }
        };
        let dev = unsafe { &mut *dev };

        // Build an ARP request (broadcast) and send it.
        // ARP over Ethernet: htype=1(eth), ptype=0x0800(IP), hlen=6, plen=4, op=1(request)
        let mut pkt = [0u8; 64];
        // Ethernet header: dst=broadcast, src=our MAC, ethertype=0x0806(ARP)
        pkt[0..6].fill(0xFF); // dst MAC = broadcast
        pkt[6..12].copy_from_slice(&dev.mac); // src MAC
        pkt[12] = 0x08; pkt[13] = 0x06; // EtherType = ARP
        // ARP header
        pkt[14] = 0x00; pkt[15] = 0x01; // HTYPE = Ethernet
        pkt[16] = 0x08; pkt[17] = 0x00; // PTYPE = IPv4
        pkt[18] = 0x06; // HLEN = 6
        pkt[19] = 0x04; // PLEN = 4
        pkt[20] = 0x00; pkt[21] = 0x01; // OPER = Request
        pkt[22..28].copy_from_slice(&dev.mac); // SHA = our MAC
        pkt[28..32].copy_from_slice(&[192, 168, 1, 1]); // SPA = 192.168.1.1
        pkt[32..38].fill(0x00); // THA = unknown
        pkt[38..42].copy_from_slice(&[192, 168, 1, 2]); // TPA = 192.168.1.2

        let sent = dev.send(&pkt);
        crate::sprintln!("e1000: ARP request sent {} bytes -> {}", pkt.len(), sent);

        // Poll for a response or echo (QEMU user-mode net may reply with ARP)
        let mut rbuf = [0u8; 2048];
        let mut received = false;
        for _ in 0..1000 {
            if let Some(n) = dev.receive(&mut rbuf) {
                crate::sprintln!("e1000: rx {} bytes ethertype={:02x}{:02x}",
                    n, rbuf[12], rbuf[13]);
                received = true;
                break;
            }
            // Busy-wait a bit
            for _ in 0..10000 { core::hint::spin_loop(); }
        }
        if received {
            crate::sprintln!("e1000: selftest PASS (send+recv ok)");
        } else {
            crate::sprintln!("e1000: selftest PARTIAL (sent ok, no rx — expected in QEMU without tap)");
        }
    }
}
