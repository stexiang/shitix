//! 网络接口桥接层 — 连接协议栈与 e1000。
//! ARP 解析、IP 发送、协议收包分发。

use crate::drivers::net::e1000::E1000;
use crate::net::inet::eth::{ETH_P_ARP, ETH_P_IP, ETH_BROADCAST};
use crate::net::inet::arp::{ARPOP_REQUEST, ARPOP_REPLY};
use crate::net::inet::ip;

const ARP_CACHE_SIZE: usize = 16;
static mut ARP_CACHE: [(u32, [u8; 6], bool); ARP_CACHE_SIZE] = [(0, [0; 6], false); ARP_CACHE_SIZE];
static mut OUR_IP: u32 = 0xC0A80101;
static mut OUR_MAC: [u8; 6] = [0; 6];

fn our_mac_ptr() -> *const u8 { unsafe { &raw const OUR_MAC as *const u8 } }
fn our_ip_val() -> u32 { unsafe { core::ptr::read_volatile(&raw const OUR_IP) } }
fn arp_cache_get(i: usize) -> (u32, [u8; 6], bool) {
    unsafe { core::ptr::read_volatile(&raw const ARP_CACHE[i]) }
}
fn arp_cache_set(i: usize, v: (u32, [u8; 6], bool)) {
    unsafe { core::ptr::write_volatile(&raw mut ARP_CACHE[i], v) }
}

pub fn init() {
    if let Some(dev) = E1000::get() {
        unsafe {
            let d = &*dev;
            core::ptr::write_volatile(&raw mut OUR_MAC, d.mac);
            let mac = core::ptr::read_volatile(&raw const OUR_MAC);
            let ip = our_ip_val();
            crate::sprintln!("netif: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} IP {}.{}.{}.{}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
                (ip>>24)&0xFF, (ip>>16)&0xFF, (ip>>8)&0xFF, ip&0xFF);
        }
    }
}

pub fn send_frame(data: &[u8]) -> i32 {
    if let Some(dev) = E1000::get() { unsafe { (*dev).send(data) } }
    else { -1 }
}

pub fn poll_receive(buf: &mut [u8]) -> Option<usize> {
    if let Some(dev) = E1000::get() { unsafe { (*dev).receive(buf) } }
    else { None }
}

pub fn arp_request(target_ip: u32) -> i32 {
    let mut pkt = [0u8; 64];
    // Ethernet header
    pkt[0..6].copy_from_slice(&ETH_BROADCAST);
    unsafe { pkt[6..12].copy_from_slice(core::slice::from_raw_parts(our_mac_ptr(), 6)); }
    pkt[12] = 0x08; pkt[13] = 0x06;
    // ARP
    pkt[14] = 0x00; pkt[15] = 0x01; pkt[16] = 0x08; pkt[17] = 0x00;
    pkt[18] = 0x06; pkt[19] = 0x04;
    pkt[20] = 0x00; pkt[21] = 0x01;
    unsafe { pkt[22..28].copy_from_slice(core::slice::from_raw_parts(our_mac_ptr(), 6)); }
    pkt[28..32].copy_from_slice(&our_ip_val().to_be_bytes());
    pkt[32..38].fill(0);
    pkt[38..42].copy_from_slice(&target_ip.to_be_bytes());
    send_frame(&pkt[..42])
}

fn arp_reply(target_mac: &[u8; 6], target_ip: u32, src_mac: &[u8; 6], src_ip: u32) -> i32 {
    let mut pkt = [0u8; 64];
    pkt[0..6].copy_from_slice(target_mac);
    pkt[6..12].copy_from_slice(src_mac);
    pkt[12] = 0x08; pkt[13] = 0x06;
    pkt[14] = 0x00; pkt[15] = 0x01; pkt[16] = 0x08; pkt[17] = 0x00;
    pkt[18] = 0x06; pkt[19] = 0x04;
    pkt[20] = 0x00; pkt[21] = 0x02;
    pkt[22..28].copy_from_slice(src_mac);
    pkt[28..32].copy_from_slice(&src_ip.to_be_bytes());
    pkt[32..38].copy_from_slice(target_mac);
    pkt[38..42].copy_from_slice(&target_ip.to_be_bytes());
    send_frame(&pkt[..42])
}

pub fn handle_arp(pkt: &[u8]) -> bool {
    if pkt.len() < 42 { return false; }
    let oper = u16::from_be_bytes([pkt[20], pkt[21]]);
    let sender_mac: [u8; 6] = pkt[22..28].try_into().unwrap();
    let sender_ip = u32::from_be_bytes(pkt[28..32].try_into().unwrap());
    let target_ip = u32::from_be_bytes(pkt[38..42].try_into().unwrap());

    // Update cache
    unsafe {
        for i in 0..ARP_CACHE_SIZE {
            let (ip, _, used) = arp_cache_get(i);
            if !used || ip == sender_ip {
                arp_cache_set(i, (sender_ip, sender_mac, true));
                break;
            }
        }
    }

    if oper == ARPOP_REQUEST && target_ip == our_ip_val() {
        let m = unsafe { core::ptr::read_volatile(&raw const OUR_MAC) };
        let our_mac_slice = [m[0], m[1], m[2], m[3], m[4], m[5]];
        arp_reply(&sender_mac, sender_ip, &our_mac_slice, our_ip_val());
    }
    true
}

pub fn arp_lookup(ip: u32) -> Option<[u8; 6]> {
    unsafe {
        for i in 0..ARP_CACHE_SIZE {
            let (cached_ip, mac, used) = arp_cache_get(i);
            if used && cached_ip == ip { return Some(mac); }
        }
    }
    None
}

pub fn arp_resolve(ip: u32) -> Option<[u8; 6]> {
    if let Some(mac) = arp_lookup(ip) { return Some(mac); }
    arp_request(ip);
    let mut rbuf = [0u8; 2048];
    for _ in 0..5000 {
        if let Some(n) = poll_receive(&mut rbuf) {
            if n >= 14 {
                let etype = u16::from_be_bytes([rbuf[12], rbuf[13]]);
                if etype == ETH_P_ARP { handle_arp(&rbuf[..n]); }
                if let Some(mac) = arp_lookup(ip) { return Some(mac); }
            }
        }
        for _ in 0..1000 { core::hint::spin_loop(); }
    }
    None
}

pub fn send_ip_packet(dst_ip: u32, proto: u8, payload: &[u8]) -> i32 {
    let dst_mac = arp_resolve(dst_ip).unwrap_or(ETH_BROADCAST);
    let ip_hdr_len: usize = 20;
    let total = ip_hdr_len + payload.len();
    let mut pkt = [0u8; 2048];

    pkt[0..6].copy_from_slice(&dst_mac);
    unsafe { pkt[6..12].copy_from_slice(core::slice::from_raw_parts(our_mac_ptr(), 6)); }
    pkt[12] = 0x08; pkt[13] = 0x00;

    let ip_start = 14;
    pkt[ip_start] = 0x45; pkt[ip_start+1] = 0x00;
    pkt[ip_start+2] = (total >> 8) as u8; pkt[ip_start+3] = total as u8;
    pkt[ip_start+4] = 0x00; pkt[ip_start+5] = 0x01;
    pkt[ip_start+6] = 0x00; pkt[ip_start+7] = 0x00;
    pkt[ip_start+8] = 64;
    pkt[ip_start+9] = proto;
    let src_ip = our_ip_val();
    pkt[ip_start+12..ip_start+16].copy_from_slice(&src_ip.to_be_bytes());
    pkt[ip_start+16..ip_start+20].copy_from_slice(&dst_ip.to_be_bytes());

    let csum = unsafe { ip::fast_csum(&pkt[ip_start] as *const u8, 5) };
    pkt[ip_start+10] = (csum >> 8) as u8;
    pkt[ip_start+11] = csum as u8;

    pkt[ip_start+ip_hdr_len..ip_start+total].copy_from_slice(payload);
    send_frame(&pkt[..14+total])
}

pub fn handle_frame(pkt: &[u8]) {
    if pkt.len() < 14 { return; }
    let etype = u16::from_be_bytes([pkt[12], pkt[13]]);
    match etype {
        ETH_P_ARP => { handle_arp(pkt); }
        ETH_P_IP => {
            if pkt.len() >= 34 {
                let proto = pkt[14+9];
                let src_ip = u32::from_be_bytes(pkt[14+12..14+16].try_into().unwrap());
                let ihl = (pkt[14] & 0x0F) as usize * 4;
                let ip_payload = &pkt[14+ihl..];
                if proto == 6 { crate::net::inet::tcp::tcp_input(src_ip, ip_payload); }
                else if proto == 17 && ip_payload.len() >= 8 {
                    // UDP：解头部后投递到 socket 层的数据报队列
                    let sport = u16::from_be_bytes([ip_payload[0], ip_payload[1]]);
                    let dport = u16::from_be_bytes([ip_payload[2], ip_payload[3]]);
                    let ulen = u16::from_be_bytes([ip_payload[4], ip_payload[5]]) as usize;
                    if ulen >= 8 && ip_payload.len() >= ulen {
                        crate::net::socket::udp_input(src_ip, sport, dport, &ip_payload[8..ulen]);
                    }
                }
            }
        }
        _ => {}
    }
}

pub fn poll() {
    let mut rbuf = [0u8; 2048];
    loop {
        match poll_receive(&mut rbuf) {
            Some(n) => handle_frame(&rbuf[..n]),
            None => break,
        }
    }
}
